# Spec 04 — Rendu de scène, chaîne d'effets et composition finale

Statut : brouillon v1 (rédaction clean-room)
Révision amont étudiée : arbre daté du 2026-10-30 (commit `a0ddce58`).
Destinataires : implémenteurs Rust (crates `lumen-render`, `lumen-scene`, `lumen-video`, `lumen-web`), qui n'ont pas accès au code amont.

## Portée

Couvert :

- boucle de rendu (horloge, cadence, pause, ordre des opérations) ;
- scène : initialisation, ordre des objets, caméra, pointeur et parallaxe ;
- objets image : géométrie, transformation hiérarchique, coordonnées de texture, maillage « puppet » ;
- chaîne de passes (matériau + effets) : construction, cibles de rendu (RT) nommées, ping-pong, liaisons de textures, états GPU, uniforms fournis par le moteur ;
- modes de blending, masque d'écriture, clears, formats ;
- bloom ;
- objets texte et son, côté rendu ;
- composition finale vers la sortie (modes fill / fit / stretch / default, clamp, inversion verticale, span multi-écrans) ;
- fonds d'écran vidéo et web, côté rendu.

Hors périmètre, traité par d'autres specs : systèmes de particules ; pilotes de sortie (X11, Wayland, fenêtre), détection de plein écran ; parsing des formats (scene.json, matériaux, effets, TEX, MDL) sauf les champs cités ici ; prétraitement et traduction des shaders WE ; moteur audio et analyse spectrale ; moteur de script.

---

## 0. Conventions de ce document

### 0.1 Vocabulaire

- **Fond** : un fond d'écran chargé (scène, vidéo ou web).
- **RT** (*render target*) : texture 2D couleur dans laquelle on peut dessiner et qu'on peut ensuite échantillonner.
- **Passe** : un dessin (un shader, des textures, un état GPU, une RT de destination).
- **Image** : un objet de scène de type image (clé `image` dans scene.json).
- **W, H** : largeur et hauteur de la projection orthographique de la scène, en unités scène (« pixels de projection »).
- **S = (Sx, Sy)** : taille d'une image en unités scène, avant échelle.
- **tronc(x)** : troncature vers zéro (conversion flottant → entier).
- Sauf mention contraire, les calculs se font en flottant 32 bits.

### 0.2 Repères

1. **Repère WE** : coordonnées des fichiers de scène. Origine au coin **bas-gauche** de la projection, x vers la droite, y vers le **haut**, la projection couvre [0, W] × [0, H].
2. **Repère centré (RC)** : utilisé pour la géométrie envoyée au GPU. `Xc = x − W/2`, `Yc = H/2 − y`. Yc croît vers le **bas** de l'image affichée.
3. **Convention de stockage** (invariant central) : dans **toutes** les textures et RT, la ligne mémoire 0 contient le **haut** de l'image, et la coordonnée de texture `v = 0` désigne cette ligne 0 ; `u = 0` désigne la colonne de gauche. Une texture chargée depuis un fichier, une image vidéo décodée, une page web rendue, le contenu de chaque RT : tout respecte cette convention.
4. **Espace clip amont** : l'implémentation amont tourne sur une API où la ligne 0 d'une RT correspond à `y_ndc = −1` et où la profondeur clip va de −1 à +1. Les matrices données dans ce document sont exprimées dans cette convention, car ce sont **ces valeurs exactes** que les shaders WE reçoivent en uniforms. Le § 24 explique comment s'en servir sur une API dont la ligne 0 correspond à `y_ndc = +1` et dont la profondeur va de 0 à 1 (wgpu).

### 0.3 Matrices de référence

- `ortho(l, r, b, t, n, f)` : projection orthographique classique « main droite, profondeur [−1, 1] » :
  - ligne 1 : `[2/(r−l), 0, 0, −(r+l)/(r−l)]`
  - ligne 2 : `[0, 2/(t−b), 0, −(t+b)/(t−b)]`
  - ligne 3 : `[0, 0, −2/(f−n), −(f+n)/(f−n)]`
  - ligne 4 : `[0, 0, 0, 1]`
- `ortho2(l, r, b, t)` = `ortho(l, r, b, t, −1, 1)` (la composante z est simplement négativée).
- `T(v)` : translation ; `Rz(θ)` : rotation autour de z, sens trigonométrique dans un repère où y monte ; `Sc(v)` : mise à l'échelle.
- `lookAt(eye, center, up)` : matrice de vue « main droite » standard (axe avant `f = normalise(center − eye)`, `s = normalise(f × up)`, `u = s × f` ; lignes `s`, `u`, `−f` ; translation `−s·eye`, `−u·eye`, `f·eye`).
- Toutes les matrices sont transmises aux shaders en ordre colonne-majeur (vecteurs colonnes, `M · v`).

---

## 1. Vue d'ensemble d'une frame

Pour chaque itération de la boucle principale (non en pause) :

1. Mise à jour des horloges (`g_Time`, `g_TimeLast`, `g_Daytime`) — § 2.
2. Mises à jour hors rendu (audio, source média, entrées souris).
3. Pour chaque sortie (écran / fenêtre) : rendu du fond associé :
   1. si le fond n'a pas encore été rendu pour ce numéro de frame : **rendu interne** du fond dans sa RT de sortie `_rt_FullFrameBuffer` (scène § 5, vidéo § 22, web § 23) ;
   2. **composition finale** : copie de `_rt_FullFrameBuffer` vers la zone de la sortie, avec le mode d'échelle et le mode de clamp configurés (§ 21).
4. Présentation, puis attente éventuelle pour respecter la cadence maximale.
5. Vérification « une application est-elle en plein écran ? » → pause éventuelle.

---

## 2. Boucle principale, temps, cadence, pause

### 2.1 Horloges

- **Horloge de rendu** : secondes écoulées depuis le démarrage du sous-système vidéo, horloge monotone (résolution au moins microseconde), exposée en flottant 32 bits. Elle ne s'arrête jamais (y compris pendant les pauses) et ne boucle pas.
- `g_Time` : valeur de l'horloge de rendu lue une fois au début de chaque frame non en pause.
- `g_TimeLast` : valeur de `g_Time` à la frame précédente ; vaut 0 avant la première frame.
- **Delta** `dt = g_Time − g_TimeLast`. À la première frame, `dt = g_Time`. Après une pause, `dt` inclut toute la durée de la pause.
- **FPS estimé** (utilisé par les scripts de texte) : `1/dt`, ou 60 si `dt ≤ 1e−6`.
- `g_Daytime` : `(heure_locale × 60 + minute) / 1440`, recalculé à chaque frame non en pause (résolution d'une minute, valeur dans [0, 1)).
- Note : l'animation des textures animées (§ 16.3) relit l'horloge de rendu au moment de la passe (pas `g_Time`) ; l'écart est négligeable mais existe.

### 2.2 Limitation de cadence

- Paramètre « FPS maximum », défaut **30** (option de ligne de commande).
- Période cible `P = 1 / fps_max`, calculée une seule fois (au premier passage) ; un changement ultérieur du réglage n'est pas pris en compte.
- Pour chaque itération : on mesure `t0` juste avant le rendu des sorties et `t1` juste après la présentation et le traitement des événements de fenêtrage. Si `t1 − t0 < P`, on dort `P − (t1 − t0)`.
- Pas d'accumulation d'erreur ni de rattrapage. Le travail fait avant `t0` (audio, média, entrées) n'est pas compté.
- Sous Wayland, le rendu de chaque sortie est en plus déclenché par les rappels de frame du compositeur (spec des pilotes de sortie) ; la limitation ci-dessus s'applique autour de l'attente d'événements.
- Les fonds web demandent au navigateur intégré une cadence de `max(60, fps_max)` images/s.

### 2.3 Compteur de frames et rendu unique

- Un compteur entier est incrémenté une fois par itération de la boucle (après présentation).
- Un fond mémorise le numéro de la dernière frame pour laquelle il a fait son rendu interne. Quand plusieurs sorties partagent le **même** fond (mode span, § 21.5), le rendu interne n'a lieu qu'une fois par numéro de frame ; chaque sortie fait ensuite sa propre composition finale.

### 2.4 Pause « plein écran »

- Activée par défaut ; désactivable par option. La détection relève d'une autre spec.
- Après le rendu d'une frame, si une application est en plein écran : on entre en pause, on mémorise l'instant, on notifie chaque fond (`pause = vrai`).
- En pause, chaque itération : dormir **250 µs** puis revérifier. Si toujours plein écran : on ne fait rien d'autre (pas de rendu, pas de mise à jour d'horloge, d'audio, d'entrées). Sinon : notifier chaque fond (`pause = faux`), décaler les minuteries de playlist de la durée de la pause (sauf si la playlist le désactive), sortir de pause ; **cette itération ne rend rien**, le rendu reprend à l'itération suivante.
- Effet de la notification par type de fond : vidéo → mise en pause / reprise du lecteur ; scène et web → aucun effet (les sons d'une scène ne sont pas suspendus par cette notification).

### 2.5 Ordre exact d'une itération non en pause

1. `g_Daytime` ; 2. `g_TimeLast ← g_Time`, `g_Time ← horloge` ; 3. mise à jour audio ; 4. mise à jour source média (pochette, etc.) ; 5. mise à jour des entrées ; 6. rendu de toutes les sorties + présentation + limitation (§ 2.2) ; 7. traitement d'une demande de fermeture ; 8. test plein écran (§ 2.4) ; 9. playlists ; 10. capture d'écran éventuelle.

---

## 3. Interface commune des fonds

Chaque fond expose :

- une **taille logique** (Pw, Ph) en pixels entiers : scène → `(tronc(W), tronc(H))` ; vidéo → taille d'affichage décodée ; web → taille de la page ;
- une RT de sortie nommée `_rt_FullFrameBuffer`, RGBA8, de la taille logique au moment de sa création, avec un alias `_rt_MipMappedFrameBuffer` (même texture, **sans** niveaux de mipmap) ;
- l'échantillonnage de cette RT suit le **mode de clamp** choisi pour la sortie (§ 21.3) ;
- une notification de pause (§ 2.4) ;
- la composition finale décrite au § 21, identique pour tous les types.

---

## 4. Scène — initialisation

### 4.1 Taille de projection

- Si `general.orthogonalprojection.auto` est faux : `W = width`, `H = height` (entiers du fichier).
- Si `auto` est vrai :
  - pour chaque objet image du fichier : `ex = |origin.x| + size.x/2`, `ey = |origin.y| + size.y/2`, avec la valeur initiale d'`origin` et la valeur `size` de l'objet (0 si absente). Échelle, alignement et parents sont ignorés ;
  - `Ex = max(ex)`, `Ey = max(ey)` (valeurs de départ 0) ;
  - si `Ex > 0` et `Ey > 0` : `W = 2·Ex`, `H = 2·Ey` ;
  - sinon : W et H = taille totale de la sortie (boîte englobant toutes les sorties connues du pilote).
- Voir Questions ouvertes (Q3) : cette formule suppose des origines centrées sur 0.

### 4.2 Caméra

Voir § 6. La caméra est construite avant les objets.

### 4.3 RT globales de la scène (ordre de création)

1. `_rt_FullFrameBuffer` : `tronc(W) × tronc(H)`, échantillonnage selon le mode de clamp de la sortie ; alias `_rt_MipMappedFrameBuffer`.
2. `_rt_shadowAtlas` : `tronc(W) × tronc(H)`, clamp-to-edge ; alias `_alias_lightCookie`. Jamais rempli (reste transparent).
3. *(création des objets, qui ajoutent leurs propres RT, § 8.9 et § 9.5)*
4. `_rt_4FrameBuffer` : `(tronc(W) div 4) × (tronc(H) div 4)` (division entière), clamp-to-edge.
5. `_rt_8FrameBuffer` : `(… div 8) × (… div 8)`, clamp-to-edge.
6. `_rt_Bloom` : même taille que `_rt_8FrameBuffer`, clamp-to-edge.

Les RT 4 à 6 existent même sans bloom, mais comme elles sont créées **après** les objets, aucun objet de la scène ne peut les référencer (la résolution des noms se fait à l'initialisation de chaque objet). Les noms WE `_rt_HalfCompoBuffer1/2`, `_rt_QuarterCompoBuffer1/2` et autres ne sont **pas** fournis : leur référence échoue (§ 12).

### 4.4 Couleur de fond (clear)

- `general.clearcolor` (vec3, défaut (1, 1, 1)) ; la RT scène est effacée à `(r, g, b, 1)` à chaque frame (§ 5).
- Autres couleurs lues : `general.ambientcolor` (défaut 0) et `general.skylightcolor` (défaut 0), transmises aux shaders (§ 13).

### 4.5 Création des objets

Type d'un objet (dans cet ordre de test) :

| Condition | Type | Comportement de rendu |
|---|---|---|
| clé `image` de type chaîne | image | § 8 à § 11 |
| clé `sound` de type tableau | son | § 19 (aucun dessin) |
| clé `particle` = objet, ou chaîne non vide différente de `"None"` | particules | autre spec ; non créé si les particules sont désactivées par option ou si le matériau manque |
| clé `text` présente | texte | § 18 |
| clé `light` ou `shape` | non supporté | objet inerte |
| autre | groupe / inconnu | objet inerte |

Un **objet inerte** ne dessine rien mais existe : il figure dans l'ordre de rendu et peut servir de parent (§ 8.1).

Algorithme de création (l'ordre compte, car les noms de RT sont résolus à la création) :

- Parcourir les objets dans l'ordre du fichier. Pour un objet pas encore créé :
  1. créer d'abord récursivement chacune de ses **dépendances** (liste `dependencies`), en ignorant l'auto-référence et les identifiants introuvables ;
  2. puis son **parent** (`parent`) ; un parent introuvable est une erreur fatale amont ;
  3. puis l'objet lui-même : construction, puis initialisation (construction de la chaîne de passes, compilation des shaders, résolution des textures).
- Si l'**initialisation** échoue (shader invalide, liaison `bind` introuvable, etc.), l'objet est journalisé et **retiré** : absent de l'ordre de rendu et des recherches de parent.
- Certaines erreurs survenant à la **construction** sont fatales pour toute la scène en amont (texture principale introuvable, RT nommée introuvable comme texture principale, image sans texture et de taille nulle). Voir Q14.
- Les cycles de dépendances provoquent une récursion infinie en amont (Q15).

### 4.6 Ordre de rendu

- Parcourir les objets dans l'ordre du fichier. Pour chaque objet **créé** : ajouter d'abord récursivement ses dépendances (pas son parent), puis l'objet, s'il n'est pas déjà dans la liste.
- Un objet non créé est sauté ; ses dépendances seront ajoutées lors de leur propre tour.
- Si le bloom est actif, l'image synthétique de bloom (§ 17) est ajoutée **en dernier**.

### 4.7 Bloom

Si `general.bloom` est vrai à l'initialisation, une image synthétique est créée après les RT 4 à 6 (§ 17).

---

## 5. Scène — déroulé du rendu interne d'une frame

1. **Pointeur** : recalculer la position virtuelle de la souris (§ 7.1) à partir de la zone d'affichage passée au rendu interne (zone de la sortie, ou `(0, 0, largeur_span, hauteur_span)` en mode span).
2. **Parallaxe** : si `general.cameraparallax` est vrai et que la parallaxe n'est pas désactivée par option, mettre à jour le déplacement (§ 7.2).
3. **Script** : un tick du moteur de script (autre spec). Les propriétés modifiées par script sont lues aux étapes suivantes.
4. **Mise à jour des textures** : pour chaque image de l'ordre de rendu (créée), appeler la mise à jour de sa **texture principale** (une texture vidéo y décode et y dessine sa trame courante ; les autres ne font rien).
5. **Clear** : RT scène liée, zone = toute la RT, effacement couleur `(clearcolor.rgb, 1)` (voir l'effet du masque d'écriture au § 11.6) ; la scène n'a pas de tampon de profondeur.
6. **Objets** : pour chaque objet de l'ordre de rendu, son rendu (filtres de diagnostic, § 25). Les objets inertes et les sons ne font rien.

---

## 6. Caméra, projection, matrices

- Seule la projection **orthographique** existe. `fov` (défaut 50) est lu mais ignoré ; pas de perspective, pas de zoom, pas de tremblement (`camerashake*` n'est exposé qu'au script), pas de `camerafade`.
- Paramètres lus en amont dans la section `camera` du scene.json : `eye`, `center`, `up` (obligatoires), `nearz` (défaut 0), `farz` (défaut 1000). Voir Q4 sur la section d'où lire `nearz`/`farz`.
- `P = ortho(−W/2, W/2, −H/2, H/2, nearz, farz) · T(eye)`.
- `V = lookAt(eye, center, up)`, calculée une fois.
- Cas canonique (`center = eye − (0, 0, d)`, `up = (0, 1, 0)`) : `V = T(−eye)` donc `P·V = ortho(−W/2, W/2, −H/2, H/2, n, f)` ; un sommet de z = 0 en RC arrive à `z_ndc = −(f+n)/(f−n)`, soit exactement −1 avec les défauts (`n = 0`). Voir § 24.2 pour la profondeur.
- Matrice d'écran d'une image : `M_ecran = P · V · R_image · T(parallaxe)` (§ 8.4 et § 8.5).

---

## 7. Pointeur et parallaxe

### 7.1 Position virtuelle du pointeur

Entrées : position souris `(px, py)` en pixels de la sortie, **origine en bas à gauche** (y vers le haut), fournie par la couche d'entrée (0, 0 si la souris est désactivée) ; zone d'affichage `(Vx, Vy, Vw, Vh)` dans le même repère ; plage UV `(u0, u1, v0, v1)` de la composition finale (§ 21.2) dans l'orientation canonique (v mesuré depuis le haut).

- `xn = clamp((px − Vx)/Vw, 0, 1)` ; `yn = clamp((py − Vy)/Vh, 0, 1)` (0 = bas).
- **Pointeur** (`g_PointerPosition`) : `(u0 + xn·(u1 − u0), v0 + (1 − yn)·(v1 − v0))`. Coordonnées de texture de la scène : (0, 0) = coin haut-gauche de la scène, (1, 1) = bas-droite, recadrage pris en compte.
- **Pointeur « normalisé »** (pour scripts et particules) : `(u0 + xn·(u1 − u0), v0 + yn·(v1 − v0))`.
- `g_PointerPositionLast` : pointeur de la frame précédente ((0, 0) au départ).
- La plage UV utilisée est celle calculée lors de la **composition précédente** (décalage d'une frame ; à la toute première frame elle vaut (0, 0, 0, 0), donc pointeur = (0, 0)).
- Souris désactivée : position (0, 0), donc pointeur = `(u0, v1)` (coin bas-gauche), ce qui produit un décalage de parallaxe constant (voir Q6).

### 7.2 Déplacement de parallaxe (global à la scène)

Paramètres `general.*` : `cameraparallaxamount` (a, défaut 1), `cameraparallaxmouseinfluence` (i, défaut 1), `cameraparallaxdelay` (d, défaut 0).

- Cible : `c = (pointeur − (0.5, 0.5)) · a · i`.
- Lissage : `k = clamp(d · dt, 0, 1)` ; `D ← D + (c − D) · k`. `D` part de (0, 0).
- Avec `d = 0`, `D` reste nul (voir Q5).

### 7.3 Application à une image

Pour chaque image, sauf si la parallaxe est désactivée par option :
`t = ((pd.x + a) · D.x · W, (pd.y + a) · D.y · W, 0)` en RC, où `pd` = `parallaxDepth` de l'image (vec2, défaut (0, 0), lu à chaque frame) et **W** sert de référence pour les deux axes.

- Le pointeur ayant v croissant vers le bas et RC ayant Yc vers le bas, l'image se déplace **dans le sens du pointeur** (pointeur à droite et en bas → image à droite et en bas).
- La translation est appliquée **avant** la rotation de l'image dans la chaîne de matrices (§ 8.6) ; le vecteur de déplacement subit donc la rotation de l'image.
- Seule la passe finale vers la scène est concernée (pas les passes intermédiaires, ni le texte, ni le bloom).

---

## 8. Objets image — géométrie

### 8.1 Transformation résolue (hiérarchie)

Transformation locale d'un nœud :

| Type du nœud | origine | échelle | angle |
|---|---|---|---|
| image | `origin` (vec3, défaut 0) | `scale` (vec3, défaut 1) | `angles.z` (radians, défaut 0) |
| texte | `origin` | `scale` | 0 |
| autre (groupe, inerte) | `origin` | `scale` | `angles.z` |

Chaîne : l'objet, son parent, le parent du parent… jusqu'à 32 niveaux au plus ; on s'arrête dès qu'un parent ne correspond à aucun objet **créé**. Composition de la racine vers l'objet : `acc` = local(racine) ; pour chaque enfant `c` en descendant :

- `off = Rot(acc.angle) · (c.origin.x · acc.scale.x, c.origin.y · acc.scale.y)`, avec `Rot(θ) = [[cos θ, −sin θ], [sin θ, cos θ]]` (repère WE, y vers le haut) ;
- `origine = (acc.origin.x + off.x, acc.origin.y + off.y, acc.origin.z + c.origin.z · acc.scale.z)` ;
- `échelle = c.scale ⊙ acc.scale` (produit composante par composante) ;
- `angle = c.angle + acc.angle`.

Le résultat (origine O, échelle E, angle θ) est recalculé à **chaque frame** (valeurs modifiables par script ou propriétés utilisateur). Taille et alignement du parent n'interviennent pas.

### 8.2 Taille de l'image

À la construction :

1. `S` = valeur `size` de l'objet (vec2, défaut (0, 0)).
2. **Texture principale** : la texture de plus petit indice déclarée dans la **première passe** du matériau du modèle (après fusion des surcharges `instance`, spec des formats). Nom commençant par `_rt_` ou `_alias_` → RT de la scène de ce nom ; sinon → texture chargée (§ 16).
3. S'il n'y a pas de texture principale : si le modèle est `solidlayer` et `S = (0, 0)`, alors `S = (W, H)` ; puis une RT vierge transparente de taille `S` (drapeaux 0 : répétition, linéaire) sert de texture principale.
4. Si `S.x = 0` ou `S.y = 0` : `S` = taille **réelle** de la texture principale (taille d'une trame pour une texture animée).
5. Si le modèle est `fullscreen` : `S = (W, H)` et l'origine résolue est **remplacée** par `(W/2, H/2, 0)` ; échelle et rotation résolues s'appliquent quand même.

`S` est figée après la construction. Les champs `width`/`height`, `autosize` et `nopadding` du modèle ne jouent aucun rôle en pratique.

### 8.3 Rectangle et alignement

- `sw = S.x · E.x`, `sh = S.y · E.y`.
- Rectangle de départ (repère WE) : `x0 = O.x − sw/2`, `x1 = O.x + sw/2`, `y0 = O.y − sh/2`, `y1 = O.y + sh/2`.
- Chaîne d'alignement : clé `horizontalalign`, sinon `alignment`, défaut `"center"`. Tests par **sous-chaîne** :
  - contient `top` → `y0, y1 −= sh/2` (l'origine désigne le bord haut) ; sinon contient `bottom` → `y0, y1 += sh/2` ;
  - contient `left` → `x0, x1 += sw/2` (l'origine désigne le bord gauche) ; sinon contient `right` → `x0, x1 −= sw/2`.
- Passage en RC : `X0 = x0 − W/2`, `X1 = x1 − W/2`, `Yh = H/2 − y1` (bord haut), `Yb = H/2 − y0` (bord bas). La composante z de l'origine est ignorée (sommets à z = 0).
- Échelle négative : le rectangle est retourné (miroir), voir l'effet sur le culling au § 11.5.

### 8.4 Rotation

- Pivot : centre du rectangle en RC, `c = ((X0 + X1)/2, (Yh + Yb)/2)` (donc **après** alignement).
- `R_image = T(c) · Rz(−θ) · T(−c)` en RC.
- Effet visible : rotation dans le sens **trigonométrique** (anti-horaire) à l'écran pour θ > 0. Seul `angles.z` compte.

### 8.5 Parallaxe

`T(parallaxe)` = `T(t)` du § 7.3 (identité si la parallaxe est désactivée par option).

### 8.6 Matrices propres à l'image

- `M_ecran = P · V · R_image · T(t)`, recalculée à chaque frame, ainsi que son inverse.
- `M_copie = ortho2(0, S.x, 0, S.y)`, ou `M_copie = M_ecran` si le modèle est `passthrough` (alors aussi recalculée à chaque frame).
- `M_passe` = identité.
- Uniform `g_ModelMatrix` (et `g_EffectModelMatrix`) = `ortho2(0, S.x, 0, S.y)` pour **toutes** les passes de l'image ; `g_ViewProjectionMatrix` = identité.

### 8.7 Coordonnées de texture de la copie initiale

`(uMax, vMax)` :

- par défaut (1, 1) ;
- texture principale **non animée** dont la taille de stockage diffère de la taille réelle (remplissage jusqu'à une puissance de 2, par exemple) : `uMax = réel_l / stockage_l`, `vMax = réel_h / stockage_h` (stockage = image 0, niveau 0). Le contenu utile est en haut à gauche du stockage (lignes 0…réel_h−1) ;
- texture **animée** : (1, 1) ; la trame est choisie dans le shader via `g_Texture0Translation` / `g_Texture0Rotation` (§ 16.3) ;
- modèle `passthrough` : (1, 1).

### 8.8 Mise à jour par frame

Avant d'exécuter ses passes, une image visible recalcule : transformation résolue, rectangle, pivot, `M_ecran` (avec parallaxe), `M_copie` si passthrough, coordonnées de copie. `S` reste constante.

### 8.9 RT composites de l'image

- Deux RT `_rt_imageLayerComposite_<id>_a` et `_rt_imageLayerComposite_<id>_b` (`<id>` = identifiant décimal de l'objet, signe compris), taille `tronc(S.x) × tronc(S.y)`, RGBA8, mêmes drapeaux d'échantillonnage que la texture principale.
- Enregistrées dans l'espace de noms **global** de la scène : tout objet initialisé ensuite peut les référencer par leur nom.
- Effacées à (0, 0, 0, 0) à la création, plus jamais ensuite.
- Notées A et B dans la suite.

---

## 9. Chaîne de passes d'une image — construction

Faite une seule fois, à l'initialisation de l'image.

### 9.1 Images passthrough sans effet

Si le modèle est `passthrough` et que l'image n'a aucun effet, ou que tous ses effets ont `visible = faux` (valeur initiale), l'image n'est **jamais initialisée** : elle reste dans l'ordre de rendu mais ne dessine jamais rien.

### 9.2 Composition de la liste (ordre)

1. **Passes de base** : une passe par passe du matériau du modèle, sans surcharge, sans liaison, sans cible. Chacune a son propre espace de noms de RT (vide), rattaché à l'image puis à la scène.
2. **Effets** (sauf diagnostic « base seule ») : pour chaque effet de l'image dans l'ordre, en sautant ceux dont `visible` vaut faux **à l'initialisation** (la visibilité des effets n'est jamais réévaluée) :
   1. créer l'espace de noms de cette instance d'effet et ses RT déclarées (§ 9.5) ;
   2. pour chaque passe du fichier d'effet, dans l'ordre :
      - passe **sans matériau** : elle doit avoir `command`, `source` et `target`. Seule la commande `copy` est supportée → passe de copie synthétique (§ 9.4). Toute autre forme est ignorée (erreur journalisée) ;
      - passe **avec matériau** : une passe par passe du matériau, toutes avec la **même** surcharge (la surcharge courante de l'instance, s'il en reste), les liaisons `bind` de la passe d'effet et sa `target` éventuelle. Ensuite seulement, le curseur de surcharge avance d'un cran.
3. **Passe de teinte de compatibilité** (§ 9.6), si l'heuristique se déclenche (sauf « base seule »).
4. **Passe colorBlendMode** (§ 9.7), si `colorBlendMode > 0` (sauf « base seule »).
5. **Transfert du mode de blending** (§ 9.8).

### 9.3 Surcharges d'instance d'effet

Le tableau `passes` d'une instance d'effet dans scene.json est consommé dans l'ordre, une entrée par passe d'effet **avec matériau** (les passes de commande n'en consomment pas). S'il y a moins d'entrées que de passes, les dernières n'ont pas de surcharge. Contenu utilisé :

- `combos` : définitions de préprocesseur, prioritaires sur celles du matériau ;
- `constantshadervalues` : prioritaires sur les constantes du matériau et les défauts du shader (§ 13.1) ;
- `textures`, `usertextures` : candidats de texture de haute priorité (§ 12.3) ;
- `id` : ignoré.

### 9.4 Passe de copie (commande `copy`)

État : blending `normal`, pas de culling, profondeur désactivée. Shader trivial : la position de sommet est émise directement en espace clip (aucune matrice), le fragment vaut l'échantillon du slot 0 à la coordonnée de texture. Slot 0 = le nom `source` (classé comme tout nom de texture, § 12.2 ; s'il est introuvable, le slot retombe sur l'entrée de la passe). Cible = `target`. C'est donc toujours une passe « avec cible » (§ 10).

### 9.5 RT déclarées par un effet

Pour chaque entrée du tableau `fbos` du fichier d'effet : `name`, `scale` (défaut 1), `format` (défaut `"rgba8888"`, **ignoré** : toujours RGBA8), `unique` (ignoré).

- Taille : `tronc(S.x / scale) × tronc(S.y / scale)`.
- Drapeaux d'échantillonnage = ceux de la texture principale de l'image.
- Effacée à la création, jamais ensuite.
- Visible uniquement depuis les passes de **cette** instance d'effet (deux effets de la même image peuvent déclarer le même nom sans conflit).

### 9.6 Passe de teinte de compatibilité (heuristique propre à l'amont)

Déclenchement : parmi les effets visibles et non exclus, dans l'ordre des effets puis des surcharges, la première surcharge qui a le combo `COMPOSITE` = 2 et une constante `compositecolor` dont la couleur (r, g, b) vérifie `r > 0.55`, `g < 0.25`, `b > 0.45`.

Action : ajout d'une passe utilisant la première passe du matériau WE `materials/effects/tint.json`, avec une surcharge : combo `BLENDMODE` = 30, constantes `color` = cette couleur, `alpha` = 1.0. Voir Q9.

### 9.7 Passe colorBlendMode

Si la valeur initiale de `colorBlendMode` (entier, défaut 0) est > 0 : ajout d'une passe utilisant la première passe du matériau WE `materials/util/effectpassthrough.json`, avec la surcharge combo `BLENDMODE` = `colorBlendMode`. La formule de mélange elle-même est dans les shaders WE. Voir § 24.6 (lecture de la RT de destination) et Q10.

### 9.8 Transfert du mode de blending

Si la liste compte plus d'une passe : la **dernière** prend le mode de blending de la **première** (celui du matériau du modèle), puis la première passe en `normal`. Le mode propre au matériau de la dernière passe est donc perdu. Si l'image a un maillage puppet, la première passe est ensuite forcée en `translucent` (§ 20).

---

## 10. Chaîne de passes — affectation des entrées et sorties

### 10.1 Algorithme

État initial : `E` (tampon d'écriture) = A ; `In` (entrée courante) = texture principale ; `Prev` = aucune ; `seq` = faux.

Pour chaque passe k (k = 0 pour la première) :

- **Passe avec cible** : nom résolu dans l'espace de l'instance d'effet, puis l'image, puis la scène. Introuvable → erreur journalisée et la passe est traitée comme **sans** cible.
  - Si `seq` est faux : `Prev ← In`, `seq ← vrai`.
  - Destination = la RT cible ; entrée = `In` ; « précédente » = `Prev`.
  - Ensuite : `In ←` la RT cible. `E` ne change pas, pas d'échange A/B.
- **Passe sans cible** :
  - Destination = `E` ; entrée = `In` ; « précédente » = `Prev` si `seq`, sinon aucune.
  - **Redirection finale** : si c'est la **dernière** passe de la liste, que l'image était visible **à l'initialisation**, et que l'option de diagnostic « pas de passe finale pour les calques pleins » ne s'applique pas : destination = RT scène (géométrie et matrice d'écran, § 10.2).
  - Ensuite : `In ← E` (le tampon qui vient d'être écrit), `E ←` l'autre tampon (A ↔ B), `seq ← faux`, `Prev ←` aucune.

Conséquences :

- une suite de passes avec cible (flou multi-étapes par exemple) chaîne ses entrées de cible en cible, et chacune voit comme « précédente » l'image telle qu'elle était **avant** la suite ; la première passe sans cible qui suit voit encore cette « précédente » ;
- si la dernière passe a une cible, rien n'est dessiné dans la scène par la voie normale (cas du bloom, § 17) ;
- une image invisible à l'initialisation qui devient visible plus tard exécute ses passes, mais sa dernière passe écrit dans A ou B, jamais dans la scène (voir Q8).

### 10.2 Géométrie, coordonnées de texture et matrice par cas

| Cas | Positions des sommets | Coord. de texture | `g_ModelViewProjectionMatrix` |
|---|---|---|---|
| Première passe (k = 0), non redirigée | rectangle [0, S.x] × [0, S.y] (pixels locaux), ou maillage puppet (§ 20) | copie : u ∈ [0, uMax], v ∈ [0, vMax] | `M_copie` |
| Première passe d'une image `passthrough` | rectangle de l'image en RC : x ∈ [X0, X1], y ∈ [Yh, Yb] ; si aussi `fullscreen` : [−1, 1] × [−1, 1] | [0, 1]² | `M_copie` (= `M_ecran`) |
| Passe k ≥ 1, non redirigée | carré [−1, 1]² | [0, 1]² | identité |
| Passe redirigée vers la scène, k ≥ 1 | rectangle de l'image en RC (§ 8.3) | [0, 1]² | `M_ecran` |
| Passe redirigée vers la scène, k = 0 (liste d'une seule passe) | rectangle de l'image en RC | copie : [0, uMax] × [0, vMax] | `M_ecran` |

`g_ModelViewProjectionMatrixInverse` reçoit l'inverse correspondante ; `g_EffectModelViewProjectionMatrix` reçoit la même valeur que `g_ModelViewProjectionMatrix`.

Correspondance des coins (invariante) : le coin de texture (0, 0) est toujours associé au **haut-gauche** de l'image, (uMax, vMax) au bas-droite. Pour la géométrie [−1, 1]², `v = (y_ndc + 1)/2` en convention clip amont ; une passe RT → RT de même taille recopie donc chaque texel à la même position (aucun retournement). Pour la copie initiale, le texel de la ligne 0 de la texture source arrive en ligne 0 de A.

Ordre des 6 sommets (2 triangles non indexés), en coins de l'image : bas-gauche, haut-gauche, bas-droite ; bas-droite, haut-gauche, haut-droite.

### 10.3 Exemples

- **Image simple** (une passe, sans effet) : passe 0 → scène, géométrie RC, texcoords de copie, blending du matériau (souvent `translucent`).
- **Image + un effet à une passe** : passe 0 (matériau, `normal`) : texture → A, géométrie de copie. Passe 1 (effet) : entrée A → scène, blending d'origine du matériau du modèle.
- **Image + effet « flou » avec RT déclarées** `t1`, `t2` et dernière passe sans cible liant `previous` au slot 0 et `t2` au slot 1 : passe 0 : texture → A ; passe 1 : A → t1 (Prev = A) ; passe 2 : t1 → t2 ; passe 3 : entrée t2, slot 0 = `previous` = A, slot 1 = t2 → scène.

---

## 11. Exécution d'une passe

### 11.1 Préconditions

Une passe sans destination ou sans entrée est sautée (erreur journalisée). En pratique cela n'arrive pas.

### 11.2 États GPU

- **Destination** : la RT ; zone de dessin = toute la RT `(0, 0, largeur, hauteur)`.
- **Blending** : § 14.
- **Profondeur** : `depthtest` `enabled` → test « inférieur ou égal », sinon désactivé ; `depthwrite` selon le drapeau. Aucune RT n'a de tampon de profondeur : ces réglages n'ont **aucun effet observable** (l'implémentation peut les ignorer).
- **Culling** : `cullmode` `normal` → élimination des faces arrière ; `nocull` → aucune (§ 11.5 pour l'orientation).
- Pas de scissor, pas de stencil.
- Défauts de matériau en amont : blending `normal`, cullmode `nocull`, depthtest `disabled`, depthwrite `disabled`. Valeur inconnue → défaut (erreur journalisée).

### 11.3 Textures

Voir § 12.3 à § 12.5 et § 16.3. Huit slots sont adressables par nom (`g_Texture0` … `g_Texture7` → slots 0 à 7).

### 11.4 Uniforms

Voir § 13.

### 11.5 Attributs, dessin, orientation des faces

- Attributs fournis : `a_Position` (vec3) et `a_TexCoord` (vec2). Tout autre attribut déclaré par un shader n'est pas alimenté (valeur par défaut (0, 0, 0, 1)).
- Dessin : 6 sommets non indexés (2 triangles), sauf maillage puppet (indexé, § 20).
- Orientation : en **coordonnées de stockage** (x vers la droite, y = indice de ligne, croissant vers le bas), les triangles émis sont parcourus dans le sens **horaire** et ce sont eux qui sont « face avant ». Avec `cullmode normal`, une image d'échelle négative sur un seul axe est donc éliminée. Voir § 24.3.

### 11.6 Masque d'écriture couleur

- Au début du rendu de chaque image : écriture des 4 canaux activée.
- Juste avant la **dernière** passe de la liste (qu'elle aille vers la scène ou vers une cible) : écriture de **R, G, B seulement** (l'alpha de la destination n'est pas modifié).
- En amont ce réglage est un état global persistant : il s'applique aussi aux opérations suivantes qui ne le redéfinissent pas (texte, clear de la scène à la frame suivante, composition finale).
- Conséquence pratique à reproduire : l'alpha de la RT scène vaut 1 (posé par le clear) et n'est jamais modifié par les compositions d'images ni par le texte ; la sortie est opaque. Recommandation : clear de la scène à alpha 1 à chaque frame, passes finales et texte en écriture RGB seule, sortie présentée opaque. Voir Q11.

---

## 12. Résolution des noms de RT et de textures

### 12.1 Espaces de noms de RT

Trois niveaux, interrogés du plus interne au plus externe :

1. l'espace de la passe : RT déclarées par l'instance d'effet (vide pour les passes de base et les passes ajoutées en 9.6 / 9.7) ;
2. l'espace de l'image (vide en pratique) ;
3. l'espace de la scène : RT globales (§ 4.3), alias, et RT composites A/B de toutes les images déjà créées.

### 12.2 Classement d'un nom

- Préfixe `_rt_` ou `_alias_` → nom de RT (§ 12.1).
- Sinon → texture chargée, via un cache **partagé par tous les fonds du processus**, indexé par le nom seul ; à la première demande, le nom est cherché successivement dans le système de fichiers virtuel de chaque fond chargé, et le premier trouvé est mis en cache. Deux fonds qui ont une texture de même nom mais de contenus différents partagent donc la première chargée (Q16).
- Noms spéciaux : `$mediaThumbnail` (pochette du média en cours) et `$mediaPreviousThumbnail` (pochette précédente), § 16.5.

### 12.3 Chaînes de candidats par slot (construites à l'initialisation)

Pour chaque slot, une liste ordonnée de candidats, de la priorité la plus faible à la plus forte :

1. texture par défaut déclarée par l'annotation d'un sampler du shader **de sommets** (clé `default`) ;
2. idem pour le shader **de fragments** ;
3. `textures` de la passe de matériau ;
4. `usertextures` de la passe de matériau ;
5. `textures` de la surcharge ;
6. `usertextures` de la surcharge ;
7. liaisons `bind` de la passe d'effet (`{index, name}`) ; le nom `previous` produit un candidat spécial « précédent ».

Un candidat dont le nom ne se résout pas (texture absente, RT inconnue) est simplement omis (erreur journalisée), **sauf** pour une liaison `bind` : un nom de liaison introuvable fait échouer l'initialisation de toute l'image (objet retiré).

### 12.4 Choix de la texture à chaque exécution

Pour chaque slot qui a des candidats, on parcourt la liste du plus prioritaire au moins prioritaire :

- candidat « précédent » : retenir `Prev` si elle existe et est prête, sinon `In` si elle est prête, sinon continuer ;
- texture réelle : la retenir si elle est prête, sinon continuer.

Si rien n'est retenu : `Prev` si elle existe et est prête, sinon `In` (même non prête).

- Slot 0 **sans** candidat : `In` (l'entrée de la passe).
- Autre slot sans candidat : rien n'est lié ; la lecture doit renvoyer (0, 0, 0, 1).
- « Prête » : RT et textures chargées toujours ; pochettes seulement quand une image non vide est chargée.
- Particularité amont du slot 0 : les candidats non prêts parcourus en tête sont définitivement retirés de la chaîne (Q12).
- Slot 0 et animation : § 16.3.

### 12.5 Résolutions de texture

- Valeur `vec4 = (largeur_stockage, hauteur_stockage, largeur_réelle, hauteur_réelle)` : RT → (l, h, l, h) ; texture chargée → stockage = image 0 niveau 0 (pour une image embarquée PNG/JPEG : taille décodée), réel = taille déclarée ; texture animée → réel = taille d'une trame ; pochette → (l, h, l, h), (1, 1, 1, 1) tant qu'aucune image n'est chargée.
- `g_Texture0Resolution` : mise à jour à **chaque exécution** depuis la texture effectivement liée au slot 0.
- `g_TextureNResolution` (N ≥ 1, et N = 0 au départ) : fixée à l'initialisation depuis le candidat le plus prioritaire du slot N. Pour un candidat « précédent », c'est la texture retenue pour le slot examiné juste avant (dans l'ordre croissant des slots, en partant de la texture principale de l'image). Q13.

---

## 13. Uniforms fournis par le moteur

### 13.1 Priorités

Pour un nom d'uniform donné, de la plus faible à la plus forte priorité :

1. défaut de l'annotation du paramètre dans le source du shader WE ;
2. constante `constantshadervalues` de la passe de matériau (associée par le nom d'alias `material` de l'annotation) ;
3. constante de la surcharge ;
4. valeurs « système » du tableau ci-dessous.

Exception : `g_CompositeColor` n'est fourni par le moteur que si le shader ne le déclare pas déjà comme paramètre annoté.

Les constantes (2) et (3) restent **liées à leur valeur source** : une propriété utilisateur qui les pilote modifie l'uniform en direct. Types supportés : float, int, vec2, vec3, vec4.

### 13.2 Tableau

| Nom | Type | Valeur | Mise à jour |
|---|---|---|---|
| `g_Texture0` … `g_Texture7` | sampler | slots 0 à 7 | fixe |
| `g_Texture0Resolution` | vec4 | § 12.5 | chaque passe |
| `g_TextureNResolution` | vec4 | § 12.5 | initialisation |
| `g_TextureReductionScale` | float | 1.0 | fixe |
| `g_Texture0Rotation` | vec4 | § 16.3 ; (0, 0, 0, 0) si non animée | chaque passe |
| `g_Texture0Translation` | vec2 | § 16.3 ; (0, 0) si non animée | chaque passe |
| `g_Time` | float | § 2.1 | chaque frame |
| `g_Daytime` | float | § 2.1 | chaque frame |
| `g_PointerPosition` | vec2 | § 7.1 | chaque frame |
| `g_PointerPositionLast` | vec2 | § 7.1 | chaque frame |
| `g_ModelViewProjectionMatrix` | mat4 | § 10.2 | chaque frame |
| `g_ModelViewProjectionMatrixInverse` | mat4 | inverse | chaque frame |
| `g_EffectModelViewProjectionMatrix` | mat4 | = `g_ModelViewProjectionMatrix` | chaque frame |
| `g_ModelMatrix`, `g_EffectModelMatrix` | mat4 | `ortho2(0, S.x, 0, S.y)` | chaque frame |
| `g_ViewProjectionMatrix` | mat4 | identité | fixe |
| `g_NormalModelMatrix` | mat3 | identité | fixe |
| `g_EffectTextureProjectionMatrix` (+ `Inverse`) | mat4 | identité | fixe |
| `g_TexelSize` | vec2 | `(1/W, 1/H)` (taille de **scène**, pas de texture) | fixe |
| `g_TexelSizeHalf` | vec2 | `(0.5/W, 0.5/H)` | fixe |
| `g_LightAmbientColor` | vec3 | `ambientcolor` | initialisation |
| `g_LightSkylightColor` | vec3 | `skylightcolor` | initialisation |
| `g_Brightness` | float | `brightness` de l'image (défaut 1) | **initialisation** |
| `g_Alpha`, `g_UserAlpha` | float | `alpha` de l'image (défaut 1) | **initialisation** |
| `g_Color` | vec3 | `color` de l'image (défaut blanc) | **initialisation** |
| `g_Color4` | vec4 | `color` en vec4 (alpha 1 par défaut) | **initialisation** |
| `g_CompositeColor` | vec3 | `color` de l'image (voir l'exception du § 13.1) | **initialisation** |
| `g_AudioSpectrum16Left` / `Right` | float[16] | spectre audio (même tableau pour les deux canaux) | chaque frame |
| `g_AudioSpectrum32Left` / `Right` | float[32] | idem | chaque frame |
| `g_AudioSpectrum64Left` / `Right` | float[64] | idem | chaque frame |

- Les valeurs marquées « initialisation » sont copiées une fois : un script ou une propriété qui change ensuite l'alpha, la couleur ou la luminosité d'une image n'a **aucun effet visible** en amont (Q7). À l'inverse, `origin`, `scale`, `angles`, `visible` et `parallaxDepth` sont relus à chaque frame.
- Ces uniforms s'appliquent à **toutes** les passes de l'image (base, effets, passes ajoutées).
- Combo de format : si la texture principale de l'image est au format RG88, toutes ses passes reçoivent `TEX0FORMAT = 8` ; si R8, `TEX0FORMAT = 9`. C'est vrai même pour les passes d'effet dont le slot 0 est une RT RGBA8 (Q17). Le reste du traitement des combos relève de la spec shaders.

---

## 14. Modes de blending

Opération = addition dans tous les cas.

| Valeur `blending` | Mélange actif | Couleur | Alpha |
|---|---|---|---|
| `normal` (défaut, et toute valeur inconnue) | oui | `src·1 + dst·0` | `src·1 + dst·0` |
| `translucent` | oui | `src·αs + dst·(1 − αs)` | `src·αs + dst·(1 − αs)` |
| `additive` | oui | `src·αs + dst·1` | `src·αs + dst·1` |

- `normal` équivaut à un remplacement.
- Texte : facteurs `translucent` (§ 18). Composition finale : mélange désactivé (§ 21).
- `colorBlendMode` n'est pas un état GPU : c'est une passe shader supplémentaire (§ 9.7).

---

## 15. Catalogue des RT, formats et clears

| RT | Taille | Échantillonnage | Effacement |
|---|---|---|---|
| `_rt_FullFrameBuffer` (+ alias `_rt_MipMappedFrameBuffer`) | taille logique du fond | mode de clamp de la sortie, linéaire | scène : à chaque frame, `(clear.rgb, 1)` ; vidéo et web : jamais |
| `_rt_shadowAtlas` (+ `_alias_lightCookie`) | W × H | clamp-to-edge, linéaire | création |
| `_rt_imageLayerComposite_<id>_a/_b` | tronc(S) | drapeaux de la texture principale | création |
| RT déclarées par un effet | tronc(S / scale) | drapeaux de la texture principale | création |
| `_rt_4FrameBuffer` | (W div 4, H div 4) | clamp-to-edge, linéaire | création |
| `_rt_8FrameBuffer`, `_rt_Bloom` | (W div 8, H div 8) | clamp-to-edge, linéaire | création |
| RT vierge d'image sans texture | S | répétition, linéaire | création |

- **Format** : toutes les RT sont RGBA 8 bits par canal, normalisées, **non sRGB**. Pas de HDR, pas de flottant, quel que soit le format déclaré.
- **Effacement à la création** : (0, 0, 0, 0).
- Aucun autre effacement par frame, sauf : la RT scène (§ 5), et la première passe d'une image à maillage puppet dont la destination n'est pas la RT scène (effacée en (0, 0, 0, 0) juste avant le dessin du maillage).
- Conséquence : une passe d'effet en `translucent` ou `additive` vers une RT qui n'est pas entièrement recouverte accumule d'une frame à l'autre. C'est le comportement amont.
- Pas de mipmaps sur les RT.

---

## 16. Textures chargées

### 16.1 Échantillonnage

Drapeaux du fichier TEX : NoInterpolation = 1, ClampUVs = 2, IsGif (animée) = 4, ClampUVsBorder = 8, Video = 32.

- **Textures chargées** : enroulement clamp-to-edge si le bit 2 est présent, sinon répétition (le bit 8 n'est **pas** honoré pour les textures chargées). Filtrage : bit 1 → agrandissement « plus proche » et réduction « plus proche, mipmap le plus proche » ; sinon agrandissement linéaire et réduction trilinéaire. Niveaux de mipmap : tous ceux présents dans le fichier.
- **RT** : bit 2 → clamp-to-edge ; sinon bit 8 → clamp-to-border avec bordure (0, 0, 0, 0) ; sinon répétition. Bit 1 → « plus proche », sinon linéaire. Pas de mipmap.
- Anisotropie maximale 8 partout où l'API l'autorise.

### 16.2 Formats

| Format TEX | Stockage GPU | Lecture shader |
|---|---|---|
| RGBA8888 | RGBA8 normalisé | (r, g, b, a) |
| DXT1 / DXT3 / DXT5 | BC1 (avec alpha) / BC2 / BC3 | idem |
| RG88 | RG8 | (r, g, 0, 1) |
| R8 | R8 | (r, 0, 0, 1) |
| image embarquée (PNG, JPEG…) | décodée en RGBA8 | (r, g, b, a) |
| autres | erreur fatale de chargement | — |

Aucune conversion sRGB : les valeurs stockées sont lues telles quelles.

### 16.3 Textures animées (bit 4)

- Trames du fichier : index d'image, durée, x, y, width1, width2, height1, height2.
- Durée totale `D` = somme des durées des trames de la **texture principale** de l'image.
- À chaque exécution d'une passe dont la texture du slot 0 est animée : `τ = fmod(horloge_de_rendu, D)` ; on parcourt les trames dans l'ordre en retranchant leur durée à τ ; la première trame pour laquelle `τ ≤ 0` est retenue (si `D = 0`, la première trame).
- Liaison : l'image de stockage indiquée par la trame est liée au slot 0.
- `g_Texture0Translation = (x/Ls, y/Hs)` ; `g_Texture0Rotation = (width1/Ls, width2/Ls, height2/Hs, height1/Hs)`, où (Ls, Hs) est la taille de stockage de l'image de la trame.

### 16.4 Textures vidéo dans une scène (bit 32 ou MP4 embarqué)

- Décodées depuis la mémoire par le lecteur vidéo, sans son (volume 0), en boucle infinie, en mode **non cadencé** : le lecteur ne se synchronise pas sur une horloge et avance au rythme des rendus (Q18).
- Lecture démarrée dès qu'au moins une image utilise la texture, arrêtée quand plus aucune ne l'utilise (compteur d'utilisation).
- Taille de stockage initiale = taille de stockage du fichier ; redimensionnée à la taille d'affichage décodée dès qu'elle est connue.
- Une trame est dessinée dans la texture à l'étape 4 du § 5, uniquement pour les textures qui sont la texture **principale** d'une image de l'ordre de rendu. Orientation : ligne 0 = haut.

### 16.5 Pochettes (`$mediaThumbnail`, `$mediaPreviousThumbnail`)

- RGBA8, clamp-to-edge, linéaire, sans mipmap.
- `$mediaThumbnail` : chargée au démarrage si disponible. À chaque changement de pochette annoncé par la source média : si la pochette courante est prête, son contenu (pixels, taille, résolution) est copié dans `$mediaPreviousThumbnail` ; puis la nouvelle est chargée.
- Prête seulement si l'image décodée est non vide.

---

## 17. Bloom

Activé si `general.bloom` est vrai à l'initialisation. Paramètres : `general.bloomstrength` (défaut 0) et `general.bloomthreshold` (défaut 0), lus une fois.

L'amont réalise le bloom avec une **image synthétique** ajoutée en dernier dans l'ordre de rendu, qui passe par la chaîne générique (§ 9 et § 10) et s'appuie sur des matériaux et shaders du dossier d'assets WE :

- identifiant réservé −1 (ses RT composites s'appellent donc `_rt_imageLayerComposite_-1_a/_b`) ; origine (W/2, H/2, 0) ; taille (W, H) ; échelle 1 ; angle 0 ; visible ; pas d'alignement particulier ;
- modèle : une passe, blending `normal`, sans culling, profondeur désactivée, shader WE `genericimage2`, texture slot 0 = `_rt_FullFrameBuffer` ;
- un effet de 4 passes, toutes avec cible :

| # | Matériau WE | Cible | Liaisons | Surcharge |
|---|---|---|---|---|
| 1 | `materials/util/downsample_quarter_bloom.json` | `_rt_4FrameBuffer` | 0 ← `_rt_FullFrameBuffer` | `bloomstrength`, `bloomthreshold` |
| 2 | `materials/util/downsample_eighth_blur_v.json` | `_rt_8FrameBuffer` | 0 ← `_rt_4FrameBuffer` | idem |
| 3 | `materials/util/blur_h_bloom.json` | `_rt_Bloom` | 0 ← `_rt_8FrameBuffer` | idem |
| 4 | `materials/util/combine.json` | `_rt_FullFrameBuffer` | 0 ← `_rt_imageLayerComposite_-1_a`, 1 ← `_rt_Bloom` | aucune |

Déroulé obtenu :

0. Passe de base : RT scène → A (copie, géométrie locale, `normal`).
1. à 3. Réduction au quart avec seuil, réduction au huitième avec flou vertical, flou horizontal (géométrie plein cadre, matrice identité, zone = toute la RT cible).
4. Combinaison : slot 0 = A (copie de la scène), slot 1 = flou → écrit dans la RT scène, blending `normal` (hérité de la passe de base par le § 9.8), écriture RGB seule (§ 11.6).

Le bloom porte donc sur tout ce qui a été dessiné avant lui dans la frame (texte compris). Ni la parallaxe ni la rotation ne l'affectent (aucune de ses passes n'utilise `M_ecran`). L'implémentation est libre de coder cette chaîne directement plutôt qu'avec une image synthétique, à condition de reproduire exactement ces 5 passes, leurs entrées, leurs tailles et leurs constantes.

---

## 18. Objets texte

Rendu propre, hors chaîne de passes : pas d'effets, pas de matériau.

### 18.1 Données utilisées

`text` (chaîne, éventuellement pilotée par un script), `font`, `pointsize` (défaut 32), `scale` (défaut 1), `color` (défaut blanc), `alpha` (défaut 1), `visible` (défaut vrai), `origin`. Les champs `horizontalalign`/`alignment`, `verticalalign`, `padding`, `size`, `angles` et `parent` sont **ignorés** au rendu.

### 18.2 Initialisation

- Texte statique vide et sans script → l'objet ne dessine jamais rien.
- Police : si `font` est non vide et ne commence pas par `systemfont_`, le fichier est lu via le système de fichiers virtuel du fond. En cas d'échec, ou pour `systemfont_*`, police système : la première trouvée parmi une liste de polices sans empattement courantes (DejaVu Sans, Liberation Sans, à leurs emplacements Linux usuels). Aucune police → l'objet ne dessine rien. Q19 pour Windows.
- Taille de rendu en pixels : `a = (scale.x + scale.y)/2` (valeurs courantes) ; `k = min(1/a, 32)` si `0 < a < 1`, sinon `k = 1` ; `px = max(1, tronc(pointsize · k))`. La police est réglée à `px` pixels de hauteur d'em.
- Texte scripté avec valeur initiale vide : on rastérise `" "` (une espace).

### 18.3 Rastérisation (sur une seule ligne)

- La chaîne est parcourue **octet par octet**, chaque octet étant pris comme code de caractère 0 à 255 (pas de décodage UTF-8). Un glyphe qui ne se charge pas est sauté.
- Glyphes rendus en couverture 8 bits antialiasée. Avance = avance horizontale du glyphe arrondie à l'entier inférieur. Pas de crénage, pas de retour à la ligne (un saut de ligne est traité comme un caractère), pas de césure.
- Largeur du bitmap = somme des avances (minimum 1). `asc` = max des hauteurs au-dessus de la ligne de base ; `desc` = max des dépassements sous la ligne de base ; hauteur = `max(1, asc + desc)`. Ligne de base à la ligne `asc` (ligne 0 = haut).
- Chaque glyphe est copié (écrasement, pas de maximum) à `x = stylo + décalage_gauche`, `y = asc − hauteur_au_dessus`, avec découpage aux bords.
- Texture : un canal (couverture), filtrage linéaire, clamp-to-edge, sans mipmap.
- Re-rastérisation chaque fois que la chaîne affichée ou `px` change (vérifié à chaque frame).

### 18.4 Dessin

- Si `visible` est faux : rien.
- Texte scripté : à chaque frame, tick du script de calque avec (temps, dt, fps estimé) ; la chaîne renvoyée est affichée (vide → `" "`).
- Quad centré, demi-tailles = (largeur_bitmap/2, hauteur_bitmap/2) en pixels de bitmap, haut du glyphe côté v = 0.
- Matrice : `P · V · T(Xc, Yc, origin.z) · Sc(scale)` avec **`Xc = origin.x − W/2` et `Yc = origin.y − H/2`**. Contrairement aux images, l'ordonnée d'origine du texte est donc comptée **depuis le haut** (Q20).
- Pas de rotation, pas de parent, pas de parallaxe.
- Fragment : `(color.rgb, color.a · alpha · couverture)`.
- Blending `translucent` (mêmes facteurs pour couleur et alpha) ; masque d'écriture hérité (RGB seul en pratique, § 11.6) ; destination : la RT scène (Q21).
- Couleur et alpha relus à chaque frame.

---

## 19. Objets son (côté rendu)

- Aucun dessin.
- À la construction, et seulement si l'audio est activé : un flux audio par fichier listé dans `sound`, en boucle si `playbackmode == "loop"`, enregistré dans le contexte audio (mixage : autre spec). `volume`, `mintime`, `maxtime` et `visible` ne sont pas utilisés.
- La pause plein écran ne suspend pas ces flux (§ 2.4).

---

## 20. Maillage puppet, squelette, modèles 3D

### 20.1 Ce qui est supporté

Seul le maillage **statique** d'un fichier puppet (`puppet` dans le JSON du modèle) attaché à une image. Squelette, poids, calques d'animation (`animationlayers`) et animations : lus mais **ignorés** (le maillage est dessiné en pose de repos). Les objets 3D, l'éclairage dynamique et la perspective ne sont pas supportés.

### 20.2 Lecture du fichier (format MDL, little-endian)

- En-tête : les 8 premiers octets valent `MDLV0021` ou `MDLV0023`, sinon pas de maillage (erreur journalisée). L'analyse commence à l'octet 9.
- Borne de fin : position de la première occurrence de `MDLS` à partir de l'octet 9, sinon la fin du fichier.
- Recherche heuristique du bloc de maillage : pour chaque décalage `o` à partir de 9, dans l'ordre croissant (tant que `o + 12 < borne`) :
  - `nv` = uint32 lu en `o + 4` : taille en octets des sommets, qui commencent en `o + 8` ;
  - conditions : `nv > 0`, `nv` multiple de 80, et l'uint32 suivant les sommets (en `o + 8 + nv`) tient avant la borne ;
  - `ni` = cet uint32 : taille en octets des indices ; conditions : `ni > 0`, multiple de 6, et les indices tiennent avant la borne ;
  - premier décalage valide retenu ; aucun → pas de maillage.
- Sommet (pas de 80 octets) : position = 3 float32 à l'octet 0 ; UV = 2 float32 à l'octet 72.
- Indices : uint16, par triangles ; un indice ≥ nombre de sommets invalide tout le maillage.

### 20.3 Rendu

- Positions dans l'espace local de l'image : `(S.x/2 + x, S.y/2 − y, z)` (y du modèle vers le haut, centre du modèle au centre de l'image) ; UV telles quelles.
- La première passe de l'image utilise ce maillage (dessin indexé) à la place du rectangle de copie, avec la matrice `M_copie`, en blending `translucent` (forcé après le § 9.8).
- Si sa destination n'est pas la RT scène : la destination est d'abord effacée en (0, 0, 0, 0).
- Cas d'une liste d'une seule passe : la passe est redirigée vers la scène avec `M_ecran`, mais l'amont dessine quand même le maillage en coordonnées **locales** (positions non converties en RC) et sans effacement préalable (Q22).

---

## 21. Composition finale vers la sortie

### 21.1 Copie

Pour chaque sortie (zone `(Vx, Vy, Vw, Vh)` de la surface) :

- dessin plein cadre dans la zone de la sortie ; mélange, profondeur et culling désactivés ; masque d'écriture hérité (RGB seul en pratique) ;
- source : `_rt_FullFrameBuffer` du fond, filtrage linéaire, sans mipmap, enroulement selon le mode de clamp ;
- dans l'**orientation canonique** (voir § 21.4), le pixel situé à la position normalisée `(s, t)` de la zone (s = 0 à gauche, t = 0 en haut) échantillonne la RT à `u = u0 + s·(u1 − u0)`, `v = v0 + t·(v1 − v0)` (v = 0 = ligne 0 = haut de l'image) ;
- la surface de sortie est effacée en début de frame par le pilote de sortie (autre spec).

### 21.2 Modes d'échelle

Entrées : taille de zone `(Vw, Vh)` (entiers), taille logique du fond `(Pw, Ph)` (entiers). Recalcul seulement quand l'une de ces tailles, ou l'orientation, change (le résultat est une fonction pure de ces entrées).

Initialisation : `u0 = 0, u1 = 1, v0 = 0, v1 = 1`.

Ajustements (calculs en flottant 32 bits ; le premier produit est tronqué en entier) :

- **Ajustement horizontal** `AH(pw, ph)` : `N = tronc(Vh / ph · pw)` ; `u0 = (N/2 − Vw/2) / N` ; `u1 = (N/2 + Vw/2) / N`. Le fond est mis à l'échelle pour que sa hauteur égale celle de la zone ; il est rogné horizontalement si `N > Vw`, ou laisse des bandes (u hors [0, 1]) si `N < Vw`.
- **Ajustement vertical** `AV(pw, ph)` : `N = tronc(Vw / pw · ph)` ; `v0 = (N/2 − Vh/2) / N` ; `v1 = (N/2 + Vh/2) / N`. Même logique sur l'autre axe.

Modes :

| Mode (option) | Règle |
|---|---|
| `stretch` | aucun ajustement : le fond est étiré sur la zone |
| `fill` | `m = max(Vw/Pw, Vh/Ph)` ; `pw = tronc(Pw·m)`, `ph = tronc(Ph·m)` ; si `pw ≠ Vw` alors `AH(pw, ph)` ; sinon si `ph ≠ Vh` alors `AV(pw, ph)` |
| `fit` | comme `fill` avec `m = min(…)` |
| `default` (défaut) | si (`Vh > Vw` et `Pw ≥ Ph`) ou (`Vw > Vh` et `Ph > Pw`) → `AH(Pw, Ph)` ; si (`Vw > Vh` et `Pw ≥ Ph`) ou (`Vh > Vw` et `Ph > Pw`) → `AV(Pw, Ph)` ; zone carrée → aucun ajustement |

- `default` : quand zone et fond ont la même orientation, ajustement sur la largeur (rognage ou bandes en haut et en bas) ; sinon, ajustement sur la hauteur.
- Les troncatures doivent être reproduites à l'identique : elles peuvent, par effet d'arrondi, faire basculer `fill`/`fit` dans l'autre branche.
- Exemple : zone 1920×1080, fond 1080×1080. `fill` → `u = [0, 1]`, `v = [0.21875, 0.78125]`. `fit` → `u ≈ [−0.3889, 1.3889]`, `v = [0, 1]`.

### 21.3 Mode de clamp

Option par sortie : `clamp` (défaut, clamp-to-edge), `border` (bordure (0, 0, 0, 0), donc bandes noires), `repeat` (répétition). Il ne règle que l'enroulement de `_rt_FullFrameBuffer` ; il agit donc aussi sur les shaders d'effet qui échantillonnent cette RT hors de [0, 1].

### 21.4 Orientation (inversion verticale)

- L'amont reçoit du pilote de sortie un booléen « inversion verticale » : vrai pour les sorties fenêtre et Wayland (orientation canonique ci-dessus), faux pour la sortie X11, qui relit l'image en mémoire.
- Avec la valeur fausse, toutes les plages v sont remplacées par `1 − v` (ordre inversé), ce qui donne une image miroir verticalement dans la surface, puis la relecture la remet à l'endroit.
- Exigence pour l'implémentation : le haut de l'image (ligne 0 de `_rt_FullFrameBuffer`) doit apparaître en haut de la sortie physique. L'inversion n'est qu'un moyen pour y parvenir, à choisir selon l'API.

### 21.5 Mode span (un fond sur plusieurs écrans)

- Boîte englobante `(Bx, By, Bw, Bh)` des sorties du groupe, en coordonnées de bureau (y vers le bas).
- Rendu interne une fois par frame avec la zone `(0, 0, Bw, Bh)` (pointeur, taille de page web).
- Plage de base `(u0, u1, v0, v1)` calculée par le § 21.2 avec `(Vw, Vh) = (Bw, Bh)`.
- Pour une sortie de position globale `(gx, gy)` et de taille logique `(lw, lh)` (mêmes unités que la boîte) : `rg = (gx − Bx)/Bw`, `rd = (gx + lw − Bx)/Bw`, `rh = (gy − By)/Bh`, `rb = (gy + lh − By)/Bh` ; plage de la sortie : `u ∈ [u0 + rg·(u1 − u0), u0 + rd·(u1 − u0)]`, `v ∈ [v0 + rh·(v1 − v0), v0 + rb·(v1 − v0)]` ; puis copie selon le § 21.1 dans la zone propre à la sortie.

---

## 22. Fond vidéo

- RT de sortie créée à la taille logique initiale **16 × 16**.
- Le lecteur vidéo (fichier physique du projet) dessine directement dans cette RT : lecture en temps réel (cadencée), boucle infinie, décodage matériel selon le réglage (défaut `auto`), aucun effacement, l'image complète est réécrite à chaque frame, ligne 0 = haut.
- Taille logique : celle annoncée par le lecteur (64 × 64 avant la première reconfiguration), puis la taille d'**affichage** décodée (rapport d'aspect des pixels appliqué). À chaque reconfiguration, la RT est réallouée à cette taille ; les UV de composition sont recalculés quand la taille change.
- Chaque rendu interne : application de la règle de sourdine, puis dessin de la trame courante.
- Son : volume = `volume_réglage × 100 / 128` (réglage 0–128, défaut 15) si l'audio est activé, sinon 0. Si l'audio est activé, la vidéo est rendue muette tant qu'une autre application joue du son, et rétablie ensuite (bascule à chaque changement d'état).
- Pause : le lecteur est mis en pause, puis repris (§ 2.4).
- Aucun effet, aucune parallaxe ; composition finale selon le § 21.

---

## 23. Fond web

- Navigateur intégré hors écran (rendu logiciel dans un tampon), page = fichier HTML du projet servi par un schéma d'URL propre au fond (spec web).
- Taille logique initiale **16 × 17** ; à chaque rendu interne, si la zone passée (sortie, ou boîte span) diffère, la taille logique devient `(Vw, Vh)` (valeurs > 0 seulement), la RT est réallouée et le navigateur est notifié du redimensionnement. En régime établi, la taille logique égale la zone, donc la composition finale est une copie 1:1 dans tous les modes.
- Peinture : chaque fois que le navigateur signale une nouvelle image (tampon BGRA 8 bits, ligne 0 = haut), elle est envoyée telle quelle dans la RT de sortie. Pas d'effacement entre deux peintures (sinon scintillement).
- Chaque rendu interne : transmission de la souris, puis une itération de la boucle de messages du navigateur (qui peut déclencher une peinture).
- Souris : `x = clamp(tronc(px − Vx), 0, Vw)` ; `y = Vh − clamp(tronc(py − Vy), 0, Vh)` (origine en haut) ; un événement de déplacement à chaque frame ; clic gauche et clic droit envoyés à chaque changement d'état (appui / relâchement, nombre de clics = 1).
- Cadence demandée au navigateur : `max(60, fps_max)`.

---

## 24. Notes de portage (API agnostique, wgpu en tête)

### 24.1 Invariant de stockage

Garder partout la convention « ligne 0 = haut, v = 0 = ligne 0 ». Les textures chargées, les trames vidéo et les pages web s'envoient sans retournement.

### 24.2 Espace clip et profondeur

Les matrices du document sont celles de l'amont (ligne 0 ↔ `y_ndc = −1`, profondeur dans [−1, 1]). Pour une API où la ligne 0 correspond à `y_ndc = +1` et où la profondeur va de 0 à 1 :

- transmettre aux shaders **exactement** les matrices décrites (les shaders WE en dépendent) ;
- à la toute fin de chaque étage de sommets (passes d'image, texte, copie), transformer la position clip `(x, y, z, w)` en `(x, −y, (z + w)/2, w)`.

Effets : la disposition mémoire est identique à l'amont ; les coordonnées de fragment (centres de pixels mesurés depuis la ligne 0) et le sens des dérivées verticales sont identiques ; les sommets à `z_ndc = −1` (cas canonique, § 6) restent dans le volume visible. Pas de tampon de profondeur à créer.

### 24.3 Orientation des faces

Avec la transformation ci-dessus, les triangles « face avant » restent ceux qui sont horaires en coordonnées de stockage (§ 11.5). Régler l'orientation avant du pipeline en conséquence (avec wgpu, cela devrait correspondre à `FrontFace::Cw` ; à confirmer par un test avec une image d'échelle négative et `cullmode normal`).

### 24.4 Composition finale

Elle n'est pas soumise à la transformation du § 24.2 : elle se définit par son résultat (§ 21.1 et § 21.4).

### 24.5 sRGB

Aucune conversion de gamma nulle part : textures et RT en formats `*Unorm` non sRGB, et surface de présentation non sRGB (ou vue non sRGB). Avec une surface sRGB, l'image serait éclaircie.

### 24.6 Lecture et écriture simultanées

En amont, certaines passes échantillonnent la RT dans laquelle elles écrivent (par exemple une passe finale d'image ou de `colorBlendMode` qui lit `_rt_FullFrameBuffer` alors que sa destination est la RT scène). wgpu l'interdit. Copier la RT de destination juste avant la passe et faire échantillonner la copie : le résultat correspond à la valeur qu'avait le pixel avant le dessin, ce que l'amont obtient en pratique.

### 24.7 Valeurs par défaut

- Slot de texture non lié : texture 1 × 1 (0, 0, 0, 1).
- Attribut non fourni : (0, 0, 0, 1).
- Échantillonneur : dérivé des drapeaux de chaque texture (§ 16.1) ; anisotropie 8 seulement si les trois filtres sont linéaires (contrainte wgpu), sinon 1.

### 24.8 États implicites

L'amont s'appuie sur des états globaux hérités (masque d'écriture, RT liée, zone de dessin, couleur de clear). Ce document décrit le comportement **effectif** qui en résulte ; l'implémentation doit expliciter ces états à chaque passe.

---

## 25. Options de diagnostic (facultatives)

Comportements amont activables par option, utiles pour comparer des rendus :

- « base seule » : pas d'effets, ni de passes 9.6 et 9.7 ;
- « pas de passe finale pour les calques pleins » : une image `solidlayer` ne compose pas sa dernière passe dans la scène ;
- journal des passes ;
- filtre « un seul objet » par identifiant ;
- liste d'objets à ne pas rendre ;
- liste d'effets à exclure à la construction.

---

## 26. Questions ouvertes

- **Q1 — Fidélité ou correction.** Plusieurs comportements amont ressemblent à des défauts (Q3, Q5 à Q9, Q12, Q20 à Q22). Faut-il les reproduire bit à bit (objectif « 1:1 ») ou viser le comportement de Wallpaper Engine sous Windows ? Proposition : reproduire l'amont par défaut et prévoir un drapeau de compatibilité par point.
- **Q2 — Couleur de clear partagée.** En amont, la couleur de clear est un état global posé à la création de chaque scène : avec plusieurs scènes (multi-écrans), la **dernière** scène créée impose sa couleur à toutes, et la surface de sortie est effacée avec cette même couleur. Reproduire, ou utiliser la couleur de chaque scène ?
- **Q3 — Projection automatique.** La formule du § 4.1 suppose des origines centrées sur 0, alors que les origines WE vont de 0 à W. Une image plein cadre en (W/2, H/2) donne une projection de 2W × 2H. Reproduire ?
- **Q4 — Source de `nearz`/`farz`/`fov`.** L'amont les lit dans la section `camera` ; elles se trouvent peut-être dans `general` dans les fichiers WE réels, auquel cas l'amont utilise toujours 0 / 1000. Avec `nearz > 0`, le cas canonique donne `z_ndc < −1`, donc un découpage complet en amont. Que faire ?
- **Q5 — Lissage de parallaxe.** Avec `cameraparallaxdelay` à 0 (défaut), la parallaxe ne bouge jamais ; avec 0,1 à 30 fps, la convergence prend environ 10 s. La sémantique WE de « delay » est probablement différente. Faut-il garder la formule ?
- **Q6 — Pointeur sans souris et sous X11.** Souris désactivée : pointeur = coin bas-gauche, d'où un décalage de parallaxe constant. Sous X11 (inversion faux), la formule du § 7.1 donne un `g_PointerPosition.y` inversé par rapport à Wayland. Quelle sémantique viser ?
- **Q7 — Alpha, couleur et luminosité figés.** `g_Alpha`, `g_Color`, `g_Brightness` et les autres sont copiés à l'initialisation : une animation par script ou propriété de ces valeurs reste invisible. Reproduire ?
- **Q8 — Visibilité figée pour la passe finale.** Une image invisible à l'initialisation ne compose jamais dans la scène, même si elle devient visible ; une image invisible n'exécute aucune passe, donc ses RT composites ne sont plus mises à jour pour les autres objets qui les lisent ; la visibilité des effets n'est évaluée qu'une fois.
- **Q9 — Heuristique de teinte magenta (§ 9.6).** Contournement spécifique à l'amont. Conserver ?
- **Q10 — colorBlendMode.** La table des valeurs de `BLENDMODE` et les textures lues par `effectpassthrough` viennent des assets WE et ne sont pas décrites ici. Il faut confirmer, avec les assets, si cette passe lit `_rt_FullFrameBuffer` (cas du § 24.6).
- **Q11 — Alpha de la sortie.** Confirmer que la sortie doit toujours être présentée opaque, y compris sous Wayland où la surface peut avoir un canal alpha.
- **Q12 — Candidats non prêts du slot 0.** L'amont retire définitivement de la chaîne du slot 0 les candidats non prêts rencontrés en tête (une pochette absente au premier rendu n'est donc plus jamais utilisée, même une fois chargée). Reproduire ?
- **Q13 — `g_TextureNResolution` pour un `bind` « previous ».** La valeur figée à l'initialisation (texture du slot examiné juste avant) est arbitraire. Faut-il plutôt la recalculer à chaque passe depuis la texture réellement liée ?
- **Q14 — Erreurs fatales.** Texture principale introuvable, parent introuvable, image sans texture et de taille nulle : l'amont fait échouer toute la scène. Faut-il plutôt ignorer l'objet ?
- **Q15 — Cycles de dépendances.** Plantage en amont. Proposition : détecter le cycle et ignorer l'arête fautive.
- **Q16 — Cache de textures global.** L'indexation par nom seul, partagée entre fonds, peut mélanger les textures de deux fonds. Faut-il indexer par fond ?
- **Q17 — `TEX0FORMAT` sur les passes d'effet.** Ce combo est déduit de la texture principale, même quand l'entrée de la passe est une RT RGBA8. Reproduire ?
- **Q18 — Vidéo non cadencée dans les scènes.** Il faut confirmer la vitesse effective (une trame par rendu ?) et décider si l'on reproduit cette vitesse ou si l'on synchronise sur l'horloge.
- **Q19 — Police système sous Windows.** Quelle liste de repli utiliser (Arial, Segoe UI…) pour rester proche de DejaVu Sans ?
- **Q20 — Ordonnée du texte.** Le texte compte son ordonnée depuis le haut, les images depuis le bas : l'un des deux est faux par rapport à WE. Il faut trancher, idéalement par comparaison avec WE Windows.
- **Q21 — Destination du texte.** L'amont dessine le texte dans la RT liée à ce moment-là, en pratique la RT scène ; si l'objet précédent finit sur une cible d'effet, le texte y atterrit. Proposition : toujours la RT scène.
- **Q22 — Puppet à une seule passe.** Le maillage est dessiné avec la matrice d'écran mais en coordonnées locales : il apparaît décalé. C'est très probablement un défaut ; faut-il le reproduire ou convertir les positions en RC ?
- **Q23 — Fonctionnalités WE absentes.** Caméra perspective, `zoom`, tremblement de caméra, lumières, HDR, formats de RT déclarés (`format` des `fbos`), `unique`, RT `_rt_HalfCompoBuffer*`. Hors cible « 1:1 amont » ; à confirmer.
- **Q24 — Précision de `g_Time`.** En flottant 32 bits, la résolution se dégrade après de longues durées (environ 16 ms après 36 h). Reproduire, ou passer en 64 bits côté CPU en ne convertissant qu'à l'envoi ?
- **Q25 — Attente en pause.** L'attente de 250 µs entre deux tests de plein écran fait tourner la boucle environ 4 000 fois par seconde. L'objectif « 1:1 » impose-t-il cette valeur, ou un intervalle plus raisonnable est-il acceptable ?
