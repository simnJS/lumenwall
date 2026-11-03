# Spec 05 — Système de particules

Statut : rédigée en clean-room à partir de l'observation du comportement de l'implémentation de référence
(révision amont `a0ddce5`, 2026-10-29). Ce document est **autosuffisant** : un implémenteur ne doit pas avoir besoin
d'autre source que les specs voisines (scène/caméra, matériaux/shaders, textures, temps global) et les fichiers
d'assets Wallpaper Engine (WE) eux-mêmes.

Conventions de notation :

- `U(a, b)` : tirage flottant uniforme dans `[a, b)` (voir §9.1 pour les détails : échange automatique si `b < a`).
- `lerp(a, b, t) = a + t·(b − a)` ; `mix` est synonyme.
- `clamp`, `normalize`, `length`, `dot`, `cross` : sens mathématique usuel, en `f32`.
- « Espace local » = repère de simulation propre au système (voir §2).
- `W`, `H` = largeur/hauteur de la projection orthographique de la scène (en pixels de scène, cf. spec caméra).
- `t_g` = temps global de l'application en secondes (`g_Time`, cf. spec temps/horloge).
- Tous les calculs sont en `f32` sauf mention contraire (le bruit de Perlin est évalué en `f64`, §9.3).

---

## 0. Portée et activation

1. Un objet de scène est un système de particules si sa clé `particle` est :
   - un objet JSON (définition inline), ou
   - une chaîne non vide différente de `"None"` (chemin vers un fichier de définition dans les assets).
   Toute autre valeur (`""`, `"None"`, absente) → l'objet n'est **pas** un système de particules
   (la détection des autres types — image, son, texte — a priorité selon la spec scène : `image` (chaîne) puis
   `sound` (tableau) puis `particle` puis `text`).
2. L'option de ligne de commande `--disable-particles` empêche la création de **tous** les systèmes de particules
   (ils n'existent pas dans le graphe de rendu ; aucune erreur).
3. Un système dont le matériau est absent ou n'a pas pu être chargé n'est **pas créé** (erreur journalisée, la scène
   continue).
4. Si l'initialisation d'un système échoue (exception), l'objet est abandonné, la scène continue.
5. Le système est dessiné directement dans le framebuffer de la scène, à sa position dans l'ordre de rendu des objets
   (entrelacé avec les images, cf. spec scène). Il n'a pas de FBO propre ni d'effets d'image.

---

## 1. Modèle de données JSON

### 1.1 Typage des valeurs

Trois familles de lecture coexistent ; il est important de respecter laquelle s'applique à chaque clé.

**(a) Valeur « utilisateur » (bindable)** — notée *[U]* dans les tableaux. Acceptée sous forme :
- nombre JSON (entier ou flottant) ;
- booléen ;
- chaîne : si elle contient 1 composante → nombre (ou texte si non numérique) ; 2/3/4 composantes séparées par des
  **espaces** → vec2/vec3/vec4 ;
- objet `{ "value": <valeur>, "user": "<nomPropriété>" | {"name":..,"condition":..}, "script": ..., "scriptproperties": ... }`
  → la valeur initiale est `value`, et la valeur suit ensuite la propriété utilisateur liée (cf. spec propriétés).

Conversions implicites (importantes) :
- un scalaire lu comme vecteur est **diffusé** sur toutes les composantes (`5` → `(5,5,5)`) ;
- un vecteur lu comme scalaire donne sa composante `x` ;
- un vec3 lu comme vec4 donne `(x, y, z, 0)` ;
- un tableau JSON `[x,y,z]` **n'est pas reconnu** pour une valeur [U] : la valeur reste nulle (`0`, `(0,0,0)`).
  (Ce cas limite doit être reproduit : il n'y a pas de repli sur le défaut.)
- clé absente → valeur par défaut indiquée.

Les valeurs [U] sont relues **à chaque utilisation** (à chaque apparition pour les initialiseurs, à chaque frame
pour les opérateurs), sauf exceptions signalées « lu une fois ».

**(b) Couleur** — notée *[C]*. Comme [U] mais une chaîne est interprétée comme couleur :
- `#RGB`, `#RGBA`, `#RRGGBB`, `#RRGGBBAA` (hexadécimal) ;
- `"r g b"` ou `"r g b a"` (virgules tolérées comme séparateurs) : si la chaîne **ne contient aucun point `.`**,
  les composantes sont des entiers 0–255 divisés par 255 ; sinon ce sont des flottants pris tels quels ;
- alpha implicite = 1.
Seules les composantes RGB sont utilisées par les particules.

**(c) Valeur brute** — notée *[B]*. Lecture directe d'un nombre/booléen JSON (pas de binding utilisateur).
Pour les champs vec3 bruts des émetteurs (`directions`, `distancemin`, `distancemax`, `origin`) et des enfants :
chaîne `"x y z"`, nombre (diffusé), ou tableau d'au moins 3 nombres ; tout autre type → défaut.
Pour `sign` : uniquement un tableau d'au moins 3 entiers, sinon défaut.

### 1.2 Clés de l'objet de scène (`scene.json`, entrée de `objects`)

| Clé | Type | Défaut | Usage |
|---|---|---|---|
| `particle` | chaîne (chemin) ou objet | — | définition du système (§1.3) |
| `origin` | vec3 [U] | `(0,0,0)` | position du système en pixels de scène ; **lu à l'initialisation et lors d'un changement de résolution seulement** |
| `scale` | vec3 [U] | `(1,1,1)` | échelle du système, relue à chaque frame |
| `angles` | vec3 [U] | `(0,0,0)` | rotation en **radians**, relue à chaque frame |
| `visible` | bool [U] | `true` | si faux : ni simulation ni rendu (§5) |
| `parallaxDepth` | vec2 [U] | `(0,0)` | profondeur de parallaxe (§2.4) |
| `instanceoverride` | objet | voir §1.5 | multiplicateurs d'instance |

`scale`, `angles`, `visible`, `parallaxDepth` sont exposées aux scripts de scène (modifiables à l'exécution).
`origin` ne l'est pas.

Le système **ne tient pas compte** de la transformation d'un éventuel objet parent (`parent`) : seule sa propre
origine/échelle/rotation compte (voir Questions ouvertes).

### 1.3 Clés du fichier de définition (racine)

Noter que les tableaux sont au **singulier** dans le format WE.

| Clé | Type | Défaut | Usage |
|---|---|---|---|
| `material` | chaîne (chemin d'un fichier matériau `.json`) | — | obligatoire en pratique (sinon système non créé) |
| `emitter` | tableau d'objets | `[]` | §6 |
| `initializer` | tableau d'objets | `[]` | §7 |
| `operator` | tableau d'objets | `[]` | §8 |
| `renderer` | tableau d'objets | `[{name:"sprite"}]` si vide/absent | §12 ; **seul le premier élément est utilisé** |
| `controlpoint` | tableau d'objets | `[]` | §13 |
| `children` | tableau d'objets | `[]` | §14 (lu mais non exploité) |
| `animationmode` | chaîne [B] | `"sequence"` | §10 |
| `sequencemultiplier` | nombre [B] | `1.0` | §10 |
| `maxcount` | entier ≥ 0 [B] | `100` | taille du pool (§4.1) |
| `starttime` | entier [B] | `0` | **ignoré** |
| `flags` | entier [B] | `0` | bit `4` = particules en perspective (§2.3) ; autres bits ignorés |

Un fichier introuvable/illisible produit un système **vide** (aucun émetteur…) dont la création échouera faute de
matériau.

Chaque entrée d'`initializer` et d'`operator` est identifiée par sa clé `name`. Un nom inconnu est **ignoré
silencieusement** (l'entrée n'existe pas). Un émetteur de `name` inconnu est ignoré (message journalisé).

### 1.4 Clés d'un émetteur

| Clé | Type | Défaut | Utilisé par |
|---|---|---|---|
| `name` | chaîne | `""` | `boxrandom` / `sphererandom` |
| `id` | entier [B] | `-1` | ignoré |
| `directions` | vec3 [B] | `(1, 1, 0)` | les deux |
| `distancemin` | vec3 [B] | `(0, 0, 0)` | les deux (sphère : composante x seulement) |
| `distancemax` | vec3 [B] | `(256, 256, 0)` | les deux (sphère : composante x seulement) |
| `origin` | vec3 [B] | `(0, 0, 0)` | les deux |
| `sign` | ivec3 [B] | `(0, 0, 0)` | sphère seulement |
| `instantaneous` | entier ≥ 0 [B] | `0` | les deux |
| `speedmin` | nombre [B] | `0` | sphère seulement |
| `speedmax` | nombre [B] | `0` | sphère seulement |
| `rate` | nombre [B] | `10` | les deux (particules/seconde) |
| `controlpoint` | entier [B] | `0` | les deux |
| `flags` | entier [B] | `0` | bit `2` : ≤ 1 particule/frame (les deux) ; bit `4` : émission périodique (boîte seulement) |
| `cone` | nombre [B] | `0` | ignoré |
| `delay` | nombre [B] (s) | `0` | boîte seulement |
| `duration` | nombre [B] (s) | `0` (= infini) | boîte seulement |
| `minperiodicdelay` / `maxperiodicdelay` | nombre [B] (s) | `1` / `2` | boîte, si bit 4 |
| `minperiodicduration` / `maxperiodicduration` | nombre [B] (s) | `2` / `3` | boîte, si bit 4 |
| `audioprocessingmode`, `audioprocessingbounds` (`(0.8,1)`), `audioprocessingexponent` (`2`), `audioprocessingfrequencystart` (`0`), `audioprocessingfrequencyend` (`1`) | — | — | **ignorés** |

### 1.5 `instanceoverride` (dans l'objet de scène)

Toutes [U]. Défauts : si la clé `instanceoverride` est absente, tous les champs valent `1` (scalaire) et `enabled`
vaut faux ; si elle est présente, champs absents = défauts ci-dessous.

| Clé | Défaut | Effet |
|---|---|---|
| `enabled` | `true` | **ignoré** |
| `alpha` | `1` | multiplie l'alpha de base et `alpharandom` à l'apparition ; fourni aussi aux uniformes `g_Alpha` et `g_UserAlpha` |
| `size` | `1` | multiplie la taille de base et `sizerandom` |
| `lifetime` | `1` | multiplie la durée de vie de base et `lifetimerandom` |
| `rate` | `1` | multiplie le `rate` des émetteurs (**lu une fois** à l'initialisation) |
| `speed` | `1` | voir liste détaillée ci-dessous |
| `count` | `1` | multiplie `maxcount` (**lu une fois** à la construction) |
| `color` | `(1,1,1)` | uniquement l'uniforme `g_Color` / `g_Color4` (§12.2) |
| `colorn` | `(1,1,1)` | multiplie la couleur de base et `colorrandom` à l'apparition |

Usages exacts de `speed` (noté `S`, relu à chaque usage) :
- multiplie : `velocityrandom`, `rotationrandom` (oui, la rotation initiale), `angularvelocityrandom`,
  `turbulentvelocityrandom`, `mapsequencearoundcontrolpoint` ;
- dans les opérateurs : terme de gravité de `movement`, intégration et force de `angularmovement`, `turbulence`,
  `vortex`, `controlpointattract`, `oscillateposition` ;
- ne multiplie **pas** : l'intégration de position de `movement`, la vitesse initiale de l'émetteur sphère, les
  opérateurs alpha/taille/couleur.

---

## 2. Repères, unités, transformations

### 2.1 Unités

- Positions : pixels de scène. Vitesses : pixels/s. Accélérations : pixels/s².
- Temps, durées de vie, délais : secondes. `rate` : particules par seconde.
- Angles (`angles` de l'objet, rotations de particules) : radians. Vitesse angulaire : rad/s.
- Fréquences des opérateurs `oscillate*` : utilisées directement comme pulsation (rad/s) dans `cos(f·t)`.
- Temps des opérateurs `*change`/`alphafade` : fractions de la durée de vie, dans `[0, 1]`.

### 2.2 Espace local et origine

Le système simule dans un espace local centré, **Y vers le haut**. Son origine dans l'espace de la scène centrée :

```
O.x = origin.x − W/2
O.y = H/2 − origin.y
O.z = origin.z
```

(même convention que les objets image, cf. spec scène). `O` est recalculé si `W` ou `H` changent entre deux
frames (§5, étape 1).

Le format JSON WE exprime plusieurs vecteurs avec Y vers le bas ; la composante Y est donc **inversée** pour
certains champs (et pas pour d'autres — reproduire exactement) :

| Champ | Y inversé ? |
|---|---|
| `origin` de l'émetteur (boîte et sphère) | oui |
| `directions` de `boxrandom` | oui |
| `directions` de `sphererandom` | **non** |
| `velocityrandom` (min/max, après tirage) | oui |
| `gravity` de `movement` | oui |
| `forward`, `right` de `turbulentvelocityrandom` | oui |
| `speedmin/speedmax` de `mapsequencearoundcontrolpoint` (après tirage) | oui |
| vitesse initiale de `sphererandom` | non (dérivée de la position déjà calculée) |
| `offset` des control points | **non** |
| `axis`, `offset` de `vortex` ; `origin` de `controlpointattract` | non |
| `mask` de `turbulence` / `oscillateposition` ; `force` de `angularmovement` | non |

### 2.3 Matrices

**Matrice modèle** `M` (appliquée aux positions locales), composée dans cet ordre (le point subit d'abord l'échelle,
puis Rx, Ry, Rz, la parallaxe, puis la translation d'origine) :

```
M = T(O) · T(P) · Rz(−angles.z) · Ry(angles.y) · Rx(−angles.x) · S(scale)
```

- `T(v)` translation ; `Rk(θ)` rotation main droite d'angle θ autour de l'axe k ; `S` échelle.
- `P` = décalage de parallaxe (§2.4), ou `0`.

**Vue-projection** `VP` :
- Si `flags & 4 == 0` (cas 2D, par défaut) : `VP = Projection_caméra · LookAt_caméra` de la scène (cf. spec caméra :
  projection orthographique `[−W/2, W/2] × [−H/2, H/2]`).
- Si `flags & 4 != 0` (perspective) : `VP = Perspective(fov, W/H, nearz, farz) · LookAt(oeil=(0,0,1000),
  cible=(0,0,0), haut=(0,1,0))`, avec `fov` en degrés (converti en radians), `nearz`, `farz` issus de la caméra de
  la scène. Convention OpenGL (profondeur NDC `[−1, 1]`).

Dérivés : `MVP = VP · M`, `MVP⁻¹`, `M⁻¹`.

### 2.4 Parallaxe

Appliquée sauf si l'option de ligne de commande `--disable-parallax` est active (cf. spec scène pour le calcul
du déplacement `D` (vec2) et de `amount`). Le booléen `enabled` de la parallaxe de la caméra n'est **pas** consulté
ici : lorsque la parallaxe est désactivée dans la scène, la scène ne met plus `D` à jour et `D` garde sa dernière
valeur (initialement `(0,0)`), ce qui donne en pratique `P = 0`.

```
d = parallaxDepth (vec2)
pour chaque composante : si |d_i| < 0.65 alors d_i = signe(d_i) · 0.65   (d_i = 0 → +0.65)
P = ( (d.x + amount) · D.x · W ,  (d.y + amount) · D.y · W ,  0 )
```

Note : les **deux** composantes utilisent la largeur `W` comme référence. La profondeur minimale de 0,65 fait qu'un
système avec `parallaxDepth = (0,0)` bouge quand même avec la parallaxe.

---

## 3. État d'une particule

Chaque particule du pool porte :

| Champ | Type | Valeur au moment de l'apparition (avant initialiseurs) |
|---|---|---|
| position | vec3 | voir émetteur |
| vitesse | vec3 | `0` (boîte) ou vitesse radiale (sphère, §6.2) |
| accélération | vec3 | `0` (jamais utilisée ensuite) |
| rotation | vec3 (rad) | `0` |
| vitesse angulaire | vec3 (rad/s) | `0` |
| accélération angulaire | vec3 | `0` (jamais utilisée) |
| couleur | vec3 | `colorn` (override) |
| alpha | f32 | `1 · override.alpha` |
| taille | f32 | `20 · override.size` |
| durée de vie | f32 (s) | `1 · override.lifetime` |
| âge | f32 (s) | `0` |
| vivante | bool | vrai |
| image (frame) | f32 | `−1` (= pas encore choisie) |
| valeurs initiales | couleur, alpha, taille, durée de vie | copies des valeurs ci-dessus |
| oscillateur alpha | {fréquence 0, échelle 1, phase 0, base 1, initialisé = faux} | réinitialisé |
| oscillateur taille | idem | réinitialisé |
| oscillateur position | {fréquence vec3 0, échelle vec3 1, phase vec3 0, initialisé = faux} | réinitialisé |

Position relative dans la vie : `life = âge / duréeDeVie` si `duréeDeVie > 0`, sinon `1`.

Une particule est vivante tant que `âge < duréeDeVie` (une durée de vie ≤ 0 meurt à la fin de sa première frame,
sans jamais être dessinée).

Les initialiseurs qui écrivent couleur/alpha/taille/durée de vie mettent aussi à jour la valeur initiale
correspondante ; `velocityrandom` et `turbulentvelocityrandom` **ajoutent** à la vitesse, les autres l'écrasent.

---

## 4. Création du système

### 4.1 Pool

```
N_pool = tronc( maxcount × override.count )     (troncature vers zéro)
si N_pool == 0 : N_pool = 1000
```

Le pool est un tableau contigu de `N_pool` emplacements ; les particules vivantes occupent les indices
`[0, n)` **dans l'ordre d'apparition** (index 0 = la plus ancienne). Aucune particule n'est créée au-delà de
`N_pool`.

### 4.2 Choix du renderer (premier élément de `renderer`)

| `name` | Mode géométrie | Combos ajoutés | Paramètres retenus |
|---|---|---|---|
| `sprite` (ou tout nom inconnu) | quads (§12.3) | — | — |
| `spritetrail` | quads (§12.3) | `TRAILRENDERER=1` | `length`, `maxlength`, `minlength` |
| `rope` | ruban (§12.4), shader `genericropeparticle` | — | `subdivision`, `uvscale`, `uvscrolling`, `uvsmoothing` |
| `ropetrail` | ruban (§12.4), shader `genericropeparticle` | `TRAILRENDERER=1` | idem `rope` + `length`, `segments` (lu mais sans effet) |

Clés d'un renderer (toutes [B]) :

| Clé | Défaut |
|---|---|
| `name` | `"sprite"` |
| `length` | `1.0` si `ropetrail`, sinon `0.05` |
| `maxlength` | `10` |
| `minlength` | `0` |
| `subdivision` | `4` si `rope`, sinon `1` (y compris `ropetrail`) ; converti en entier tronqué, borné à ≥ 0 |
| `segments` | `4` (borné à ≥ 2 pour `ropetrail`) — **sans effet** |
| `uvscale` | `1` |
| `uvscrolling` | `false` |
| `uvsmoothing` | `true` |
| `fadealpha`, `fadesize` | `false` — **ignorés** |

Paramètres de traînée effectifs (envoyés au shader via `g_RenderVar0`, §12.2) :
- `spritetrail` : `(length, maxlength, minlength)` du JSON ;
- `ropetrail` : `(length, 10, 0)` ;
- autres : `(0.05, 10, 0)`.

### 4.3 Données de sprite sheet

Lues depuis la texture principale du matériau (§12.1, cf. spec textures) : nombre de colonnes `C`, de lignes `R`,
d'images `F`, durée totale d'un cycle `T_anim` (s). Origine de ces valeurs :
1. Texture animée (format TEX avec frames) : `C = arrondi(largeurImage / largeurFrame0)`,
   `R = arrondi(hauteurImage / hauteurFrame0)`, `F = nombre de frames`, `T_anim = Σ durées des frames` —
   **seulement si** `C > 0`, `R > 0` et `C·R ≥ F` (sinon pas de sprite sheet : texture animée « type GIF », gérée par
   la spec textures).
2. Fichier compagnon `<texture>.tex-json` s'il existe, clé `spritesheetsequences` (tableau non vide) : on lit le
   **premier** élément `{frames, width, height, duration (défaut 1)}` ; si `frames > 0`, `width > 0`, `height > 0`
   (et dimensions d'image > 0) : `C = arrondi(largeurImage / width)`, `R = arrondi(hauteurImage / height)`,
   `F = frames`, `T_anim = duration`. Ce fichier a priorité sur (1).
3. Sinon `F = 0` (pas de sprite sheet).

Le mode sprite sheet est actif ssi `F > 0` (combo `SPRITESHEET=1`, §12.1).

### 4.4 Control points

8 emplacements (indices 0–7), tous initialisés à position `(0,0,0)`, offset `(0,0,0)`, non liés à la souris.
Pour chaque entrée de `controlpoint` dont `id ∈ [0, 8)` :

| Clé | Type | Défaut |
|---|---|---|
| `id` | entier [B] | `-1` (→ entrée ignorée) |
| `flags` | entier [B] | `0` ; bit `1` = lié à la souris ; bit `2` = « world space » |
| `offset` | chaîne `"x y z"` (séparateurs blancs ; composantes manquantes → 0) ou nombre (diffusé) ; tout autre type (tableau, objet) → `(0,0,0)` ; non bindable | `(0,0,0)` |
| `locktopointer` | bool [B] | `false` — **ignoré** (seul le bit 1 de `flags` compte) |

Position initiale d'un control point **non lié** à la souris :
- bit 2 levé (world space) : `position = offset − O` ;
- sinon : `position = offset`.
(`offset` n'est **pas** inversé en Y.) Un point lié à la souris est mis à jour à chaque frame (§13).

Une entrée ultérieure avec le même `id` écrase la précédente.

### 4.5 Générateur aléatoire

Un seul générateur pseudo-aléatoire **par système**, partagé par ses émetteurs, initialiseurs et opérateurs.
L'implémentation de référence utilise un Mersenne Twister 32 bits graine depuis l'entropie du système d'exploitation :
la simulation n'est donc **pas déterministe** d'une exécution à l'autre. L'implémenteur est libre de l'algorithme
(qualité statistique raisonnable exigée) ; il est recommandé de permettre une graine fixe pour les tests.
L'ordre des tirages documenté ci-dessous n'a d'importance que pour la reproductibilité interne.

Tirages effectués **à l'initialisation** : uniquement par l'opérateur `turbulence` (§8.7).

---

## 5. Boucle par frame (ordre exact)

À chaque frame où l'objet est rendu (ordre de rendu de la scène) :

1. Si le système n'est pas initialisé ou si `visible` est faux → **rien** (ni simulation ni dessin ; le temps de
   référence du système n'avance pas).
2. **Première frame** : si le temps de référence `t_ref` vaut exactement `0`, poser `t_ref = t_g`, tenter le dessin
   (le pool est vide, rien n'est dessiné) et terminer la frame. (Cas limite : si `t_g` vaut lui-même 0, la frame
   suivante est encore traitée comme « première ».)
3. `dt = t_g − t_ref` ; `t_ref = t_g`. Si `dt > 0` : `dt = min(dt, 0.1)` puis simuler (étapes 4–11). Si `dt ≤ 0`,
   pas de simulation.
   Conséquence : après une période d'invisibilité ou une frame très longue, la simulation n'avance que de 0,1 s ;
   sous 10 FPS, la simulation ralentit.
4. **Changement de résolution** : si `(W, H)` diffère de la dernière valeur connue, recalculer `O` (§2.2) et, pour
   chaque control point non lié à la souris et world space, `position = offset − O`.
5. **Control points souris** : mise à jour (§13).
6. **Émetteurs**, dans l'ordre du JSON : chacun crée ses particules et applique immédiatement **tous** les
   initialiseurs (dans l'ordre du JSON) à chaque nouvelle particule (§6).
7. **Vieillissement** : `âge += dt` pour toutes les particules `[0, n)`, y compris celles nées à l'étape 6
   (une particule est donc dessinée pour la première fois avec `âge = dt`).
8. **Opérateurs**, dans l'ordre du JSON, chacun sur toutes les particules `[0, n)` (§8). Les opérateurs voient
   `t_g` (temps absolu, s) et `dt`.
9. **Sprite sheet** : mise à jour de l'image courante (§10) si `F > 0`.
10. **Mort et compaction** : retirer toutes les particules avec `âge ≥ duréeDeVie`, en **préservant l'ordre** des
    survivantes (compactage stable vers le début du tableau).
11. (fin de simulation)
12. **Dessin** si `n > 0` (et `n ≥ 2` pour les ropes) : construction de la géométrie et rendu (§12).

---

## 6. Émetteurs

### 6.1 Logique commune

- Si `controlpoint == −1` explicitement **et** que la liste `controlpoint` du JSON est non vide **et** que sa
  *première entrée* (dans l'ordre du fichier, quel que soit son `id`) a le bit 1 (souris) : l'émetteur utilise le
  control point d'indice 0. Sinon il utilise l'indice indiqué (défaut **0**).
- Origine d'apparition : `E = (origin.x, −origin.y, origin.z) + CP[idx].position` si `idx ∈ [0, 8)`, sinon sans
  terme de control point. **Conséquence** : comme le défaut est 0, tout émetteur est décalé par la position du
  control point 0 (nulle si non défini, l'offset de CP0 sinon, la souris si lié).
- `rate_eff = rate × override.rate` (lu une fois à l'initialisation).
- Si le pool est plein (`n ≥ N_pool`) au début de l'appel, l'émetteur **ne fait rien du tout** : ni particule ni
  avancement de ses minuteries/accumulateurs (délais, durée, périodicité, accumulateur de taux) — ils sont gelés.
- Les particules sont créées une par une à l'indice `n`, puis `n += 1`, tant que `n < N_pool` ; ce qui ne rentre
  pas est perdu (y compris le reliquat d'une rafale instantanée).
- Après le calcul de position (et vitesse pour la sphère), la particule reçoit les valeurs de base du §3, puis les
  initialiseurs.

### 6.2 `boxrandom`

État propre à l'émetteur (initialisé à la création) : accumulateur `acc = 0`, délai restant `= delay`,
chrono de durée `= 0`, chrono périodique `= 0`, durée périodique courante `= 0`, délai périodique courant `= 0`,
`en_émission = faux`, `rafale_faite = faux`.

À chaque frame (après le test « pool plein ») :

1. **Délai** : si le délai restant est `> 0`, le décrémenter de `dt` et s'arrêter pour cette frame (la frame où il
   devient ≤ 0 n'émet rien ; l'excédent est perdu).
2. **Durée** : si `duration > 0`, ajouter `dt` au chrono de durée ; s'il est `≥ duration`, s'arrêter (définitivement).
3. **Périodique** (si `flags & 4`) : ajouter `dt` au chrono périodique.
   - Si pas en émission : si chrono `≥` délai périodique courant (0 au départ, donc démarrage immédiat) → passer en
     émission, remettre le chrono à 0, tirer la durée courante `U(minperiodicduration, maxperiodicduration)` et
     **émettre normalement cette frame** ; sinon s'arrêter.
   - Si en émission : si chrono `≥` durée courante → sortir d'émission, chrono à 0, tirer le délai courant
     `U(minperiodicdelay, maxperiodicdelay)`, et s'arrêter pour cette frame.
4. **Nombre à émettre** `k = 0` :
   - si `instantaneous > 0` et rafale non faite : `k = instantaneous`, rafale faite ;
   - si `rate > 0` (valeur brute du JSON) : `acc += dt × rate_eff` ; `m = tronc(acc)` ; `acc −= m` ; si `flags & 2`
     et `m > 1` → `m = 1` (l'excédent est jeté) ; `k += m`.
5. Pour chacune des `k` particules (dans la limite du pool) :
   - pour chaque axe dans l'ordre x, y, z : `d = U(distancemin_a, distancemax_a)` puis `s = U(0, 1)` ; si `s < 0.5`,
     `d = −d`. (Deux tirages par axe : la position est symétrique autour de l'origine ; avec `distancemin > 0` on
     obtient une boîte creuse.)
   - multiplier composante par composante par `(directions.x, −directions.y, directions.z)` ;
   - `position = E + résultat` ; vitesse nulle ;
   - valeurs de base (§3), puis initialiseurs.

Ignorés par la boîte : `sign`, `speedmin`, `speedmax`, `cone`.

### 6.3 `sphererandom`

État : accumulateur `acc = 0`, rafale restante `r = instantaneous`. La durée de vie de base est figée à
l'initialisation (`1 × override.lifetime` lu une fois). Pas de gestion de `delay`, `duration` ni périodicité.

À chaque frame (après le test « pool plein ») :

1. `acc += dt × rate_eff` (toujours, même si `rate = 0`) ; `k = tronc(acc)` ; `acc −= k` ; si `flags & 2` et `k > 1`
   → `k = 1`.
2. Si `r > 0` : `k = r` (**remplace** le nombre issu du taux, ne s'y ajoute pas) ; `r = 0`.
3. Pour chaque particule :
   - `rmin = distancemin.x`, `rmax = distancemax.x` (les composantes y, z sont ignorées).
   - **Cas 2D** (`flags` du système `& 4 == 0`) : tirer `θ = U(0, 2π)`, puis `ρ = √U(rmin², rmax²)` (répartition
     uniforme en surface dans l'anneau), puis `z = U(−rmax, rmax)` ;
     `p = (ρ cos θ, ρ sin θ, z)`, puis `p *= directions` (composante par composante, **sans** inversion Y).
     Avec `directions` par défaut `(1,1,0)`, `z` est annulé.
   - **Cas perspective** (`& 4 != 0`) : tirer `φ = U(0, 2π)`, `c = U(−1, 1)`, `s = √(1 − c²)` ;
     `u = (s cos φ, s sin φ, c)` (direction uniforme sur la sphère) ; puis `ρ = ∛U(rmin³, rmax³)` (uniforme en
     volume dans la coquille) ; `p = u × ρ × directions`.
   - **Signe** : pour chaque axe a, si `sign_a == 1` → `p_a = |p_a|` ; si `sign_a == −1` → `p_a = −|p_a|` ;
     sinon inchangé.
   - `position = E + p`.
   - **Vitesse** : si `speedmax > 0` ou `speedmin ≠ 0` : direction `= normalize(p)` (ou `(0,1,0)` si `p` est nul),
     vitesse `= direction × U(speedmin, speedmax)` (non multipliée par `override.speed`) ; sinon vitesse nulle.
   - valeurs de base (§3, durée de vie figée), puis initialiseurs.

Ignorés par la sphère : `delay`, `duration`, bit `4` de `flags`, `cone`.

---

## 7. Initialiseurs

Appliqués à chaque nouvelle particule, dans l'ordre du JSON, juste après les valeurs de base.
Clé `name` → comportement. Tous les paramètres sont [U] sauf mention [C].

### 7.1 `colorrandom`

| Clé | Défaut |
|---|---|
| `min` [C] | noir `(0,0,0)` |
| `max` [C] | blanc `(1,1,1)` |

`couleur = (U(min.r, max.r), U(min.g, max.g), U(min.b, max.b)) × colorn` — trois tirages **indépendants** par canal
(ce n'est pas une interpolation sur un même `t`). Met à jour la couleur initiale.

### 7.2 `sizerandom`

| Clé | Défaut |
|---|---|
| `min` | `0` |
| `max` | `20` |
| `exponent` | `1` |

`t = U(0,1)` ; `taille = (min + t^exponent × (max − min)) × override.size / 2`. Met à jour la taille initiale.
**Attention** : division par 2 ici, alors que la taille de base (sans initialiseur) vaut `20 × override.size` sans
division.

### 7.3 `alpharandom`

`min` = `0.05`, `max` = `1`. `alpha = U(min, max) × override.alpha`. Met à jour l'alpha initial.

### 7.4 `lifetimerandom`

`min` = `0`, `max` = `1`. `duréeDeVie = U(min, max) × override.lifetime`. Met à jour la durée initiale.
Effet de bord à l'initialisation du système : si `min == max` (valeurs lues à ce moment), le système est marqué
« durées de vie uniformes » (utilisé par le lissage UV des ropes, §12.4). S'il y a plusieurs `lifetimerandom`, le
dernier de la liste décide. Sans `lifetimerandom`, le marqueur est faux.

### 7.5 `velocityrandom`

`min` = `(−32, −32, −32)`, `max` = `(32, 32, 32)`.
`v = (U(min.x,max.x), U(min.y,max.y), U(min.z,max.z)) × S` ; `v.y = −v.y` ; `vitesse += v` (**additif**).

### 7.6 `rotationrandom`

`min` = `(0, 0, 0)`, `max` = `(0, 0, 2π)`.
`rotation = (U(min.x,max.x), U(min.y,max.y), U(min.z,max.z)) × S` (écrase ; la multiplication par `S` est voulue
par compatibilité).

### 7.7 `angularvelocityrandom`

`min` = `(0, 0, −5)`, `max` = `(0, 0, 5)`, `exponent` = `1`.
Pour chaque axe a (x, y, z) : `t = U(0,1)^exponent` ; `ω_a = min_a + t × (max_a − min_a)`.
`vitesseAngulaire = ω × S` (écrase).

### 7.8 `turbulentvelocityrandom`

| Clé | Défaut |
|---|---|
| `speedmin` | `100` |
| `speedmax` | `250` |
| `scale` | `1` |
| `offset` | `0` (radians) |
| `forward` | `(0, 1, 0)` |
| `right` | `(0, 0, 1)` |
| `timescale` | `1` |
| `phasemin` | `0` |
| `phasemax` | `0.1` |

Algorithme pour une particule (position `x` déjà fixée par l'émetteur et les initialiseurs précédents) :

1. `f = forward` avec `f.y` inversé ; `r = right` avec `r.y` inversé. Normaliser chacun si sa longueur `> 1e−4`,
   sinon remplacer respectivement par `(0,1,0)` et `(1,0,0)`.
2. `v = U(speedmin, speedmax)` (1er tirage).
3. `q = 0.1 × x + (t_g × timescale) × (1,1,1)`.
4. `φ = U(phasemin, phasemax)` (2e tirage) ; `q += (φ, 0.7 φ, 1.3 φ)`.
5. `d = curl(q)` (§9.4). Si `|d| < 1e−4` → `d = f`, sinon `d = d / |d|`.
6. **Cône** : si `scale < 2` : `α = acos(clamp(dot(d, f), −1, 1)) / π` (angle normalisé dans `[0,1]`) ;
   `α_max = scale / 2`. Si `α > α_max` **et** `α_max > 1e−4` : `axe = cross(d, f)` ; si `|axe| > 1e−4`, le
   normaliser et faire tourner `d` autour de `axe` d'un angle `(α − α_max) × π` (rotation main droite, formule de
   Rodrigues), ce qui ramène `d` sur le bord du cône d'axe `f`.
   Cas limite à reproduire : `scale = 0` ⇒ `α_max = 0` ⇒ **aucune contrainte** (direction libre).
7. **Inclinaison** : si `|offset| > 1e−4`, faire tourner `d` autour de `r` d'un angle `−offset` (Rodrigues).
8. **Projection 2D** : si le système n'est pas en perspective (`flags & 4 == 0`) : `d.z = 0`, puis renormaliser si
   la longueur restante `> 1e−4` (sinon laisser tel quel).
9. `vitesse += d × v × S` (**additif**).

### 7.9 `mapsequencearoundcontrolpoint`

| Clé | Défaut |
|---|---|
| `controlpoint` | `0` (lu comme nombre, tronqué en entier) |
| `count` | `1` (tronqué en entier) |
| `speedmin` | `(0, 0, 0)` |
| `speedmax` | `(100, 100, 100)` |

État : compteur de séquence `k = 0`, propre à cette entrée d'initialiseur et persistant pendant toute la vie du
système.

1. `θ = (k / count) × 2π` ; puis `k = (k + 1) mod count`.
2. `centre = CP[controlpoint].position` si l'indice est dans `[0, 8)`, sinon `(0,0,0)`.
3. `position = centre` (**écrase** la position de l'émetteur, origine d'émetteur comprise).
4. `s = (U(speedmin.x,speedmax.x), U(speedmin.y,speedmax.y), U(speedmin.z,speedmax.z))` ; `s.y = −s.y`.
5. Rotation **horaire** de `θ` dans le plan XY :
   `s' = ( s.x cos θ + s.y sin θ ,  −s.x sin θ + s.y cos θ ,  s.z )`.
6. `vitesse = s' × S` (écrase).

`count ≤ 0` est indéfini (division/modulo par zéro) : voir Questions ouvertes ; recommandation : traiter comme 1.

---

## 8. Opérateurs

Appliqués chaque frame après le vieillissement, dans l'ordre du JSON, à toutes les particules `[0, n)`.
Tous les paramètres sont [U] et relus à chaque frame, sauf mention.

### 8.1 Fonction de rampe

Notée `rampe(life, t0, t1, v0, v1)` :
- `life ≤ t0` → `v0` ;
- sinon `life ≥ t1` → `v1` ;
- sinon `v0 + (life − t0)/(t1 − t0) × (v1 − v0)`.
(Le test `≤ t0` est fait en premier : si `t1 < t0`, la valeur saute de `v0` à `v1` sans interpolation.)

### 8.2 `movement`

`drag` = `0`, `gravity` = `(0, 0, 0)`.
Avec `g = (gravity.x, −gravity.y, gravity.z)`, pour chaque particule, **dans cet ordre** :
1. `position += vitesse × dt` (avec la vitesse de la frame précédente) ;
2. `vitesse += g × dt × S` ;
3. `vitesse *= max(0, 1 − drag × dt)`.

Notes : sans opérateur `movement`, la vitesse n'affecte **jamais** la position (seul `oscillateposition` déplace
alors les particules). Deux opérateurs `movement` intègrent deux fois la position.

### 8.3 `angularmovement`

`drag` = `0`, `force` = `(0, 0, 0)` (pas d'inversion Y).
1. `rotation += vitesseAngulaire × dt × S` ;
2. `vitesseAngulaire += force × dt × S` ;
3. `vitesseAngulaire *= max(0, 1 − drag × dt)` ;
4. ramener chaque composante de `rotation` dans `[−π, π]` par ajouts/retraits répétés de `2π`.

### 8.4 `alphafade`

`fadeintime` = `0.5`, `fadeouttime` = `0.5` (fractions de vie).
- si `life ≤ fadeintime` : `alpha = alphaInitial × rampe(life, 0, fadeintime, 0, 1)` ;
- sinon si `life > fadeouttime` : `alpha = alphaInitial × (1 − rampe(life, fadeouttime, 1, 0, 1))` ;
- sinon : `alpha = alphaInitial`.
Puis `base de l'oscillateur alpha = alpha`.

### 8.5 `sizechange` / `alphachange`

Défauts communs : `starttime` = `0`, `endtime` = `1`, `startvalue` = `1`, `endvalue` = `0`.
- `sizechange` : `taille = tailleInitiale × rampe(life, starttime, endtime, startvalue, endvalue)` ; puis
  `base de l'oscillateur taille = taille`.
- `alphachange` : `alpha = alphaInitial × rampe(...)` ; puis `base de l'oscillateur alpha = alpha`.

### 8.6 `colorchange`

`starttime` = `0`, `endtime` = `1`, `startvalue` = `(1,1,1)`, `endvalue` = `(1,1,1)` — valeurs **[U] vec3
flottantes** (pas de conversion 0–255 : `"255 0 0"` vaut 255).
`couleur = couleurInitiale × (rampe_r, rampe_g, rampe_b)` avec une rampe par canal.

### 8.7 `turbulence`

| Clé | Défaut |
|---|---|
| `scale` | `0.005` |
| `speedmin` | `500` |
| `speedmax` | `1000` |
| `timescale` | `0.01` |
| `mask` | `(1, 1, 0)` |
| `phasemin` | `0` |
| `phasemax` | `0` |
| `audioprocessingmode` (0), `audioprocessingbounds` ((0,1)), `audioprocessingexponent` (1), `audioprocessingfrequencystart` (0), `audioprocessingfrequencyend` (15) | ignorés |

**À l'initialisation du système (une seule fois par opérateur)** : `φ = U(phasemin, phasemax)` puis
`V = U(speedmin, speedmax)` (dans cet ordre). Ces deux valeurs sont figées pour toute la vie du système et
partagées par toutes les particules.

Chaque frame :
- si `V ≤ 1e−4` : l'opérateur ne fait rien ;
- `k = scale × 2` ;
- pour chaque particule : `q = position` ; `q.x += φ + timescale × t_g` ; `q *= k` ; `c = curl(q)` ;
  si `|c| > 1e−4` alors `c = c / |c| × V` (sinon `c` est laissé tel quel, quasi nul) ;
  `c *= mask` ; `vitesse += c × dt × S`.

### 8.8 `vortex` (et alias `vortex_v2`)

| Clé | Type | Défaut |
|---|---|---|
| `controlpoint` | entier [B] | `0` |
| `flags` | entier [B] | `0` ; bit 1 = axe infini (cylindre), bit 2 = force centripète, bit 4 = anneau |
| `axis` | [U] vec3 | `(0, 0, 1)` |
| `offset` | [U] vec3 | `(0, 0, 0)` |
| `distanceinner` | [U] | `500` |
| `distanceouter` | [U] | `650` |
| `speedinner` | [U] | `2500` |
| `speedouter` | [U] | `0` |
| `centerforce` | [U] | `1` |
| `ringradius` | [U] | `300` |
| `ringwidth` | [U] | `50` |
| `ringpulldistance` | [U] | `50` |
| `ringpullforce` | [U] | `10` |
| `audioprocessingmode` | [U] (lu une fois) | `0` |
| `audioprocessingbounds` | [U] | `(0, 1)` — ignoré |

Si `audioprocessingmode > 0` (lu à l'initialisation) : l'opérateur est **entièrement inactif** (l'audio n'étant pas
implémenté, l'amplitude est considérée nulle).

Chaque frame :
- `centre = CP[controlpoint].position + offset` si l'indice est dans `[0, 8)`, sinon `offset` ;
- `a = normalize(axis)`, ou `(0,0,1)` si `axis` est nul.

Pour chaque particule :
1. `w = position − centre`. Si axe infini : `ρ⃗ = w − a × dot(w, a)` ; sinon `ρ⃗ = w`. `d = |ρ⃗|`.
2. `τ = cross(a, ρ⃗)` ; si `|τ| ≤ 0.001` → particule ignorée ; sinon `τ = normalize(τ)`.
   (Avec `a = (0,0,1)`, la rotation est anti-horaire dans l'espace local Y-haut.)
3. Vitesse de rotation `v` et force radiale `F` (initialement nulle) :
   - **Mode anneau** (bit 4) : `in = ringradius − ringwidth/2`, `out = ringradius + ringwidth/2` ;
     - `d < in` → `v = 0` ;
     - `d ≤ out` → `v = lerp(speedinner, speedouter, (d − in)/ringwidth)` ;
     - `d ≤ out + ringpulldistance` → `p = (d − out)/ringpulldistance` ; `v = speedouter × (1 − p)` ; si
       `d > 0.001`, `F = −normalize(ρ⃗) × ringpullforce × p` (la traction **croît** avec l'éloignement de l'anneau) ;
     - au-delà → `v = 0`.
   - **Mode standard** : `m = distanceouter − distanceinner + 0.1` ;
     - si `m < 0` ou `d < distanceinner` → `v = speedinner` ;
     - sinon si `d > distanceouter` → `v = speedouter` ;
     - sinon `v = lerp(speedinner, speedouter, (d − distanceinner)/m)`.
4. `vitesse += τ × v × dt × S` ; `vitesse += F × dt × S`.
5. Si bit 2 et `d > 0.001` : `vitesse += −normalize(ρ⃗) × centerforce × dt × S`.

### 8.9 `controlpointattract`

| Clé | Type | Défaut |
|---|---|---|
| `controlpoint` | entier [B] | `0` |
| `origin` | [U] vec3 | `(0,0,0)` |
| `scale` | [U] | `100` |
| `threshold` | [U] | `1000` |

Si l'indice n'est pas dans `[0, 8)` : inactif. Sinon `c = CP.position + origin`, `seuil = threshold / 2`.
Pour chaque particule : `δ = c − position`, `d = |δ|` ; si `0.001 < d < seuil` :
`vitesse += (δ / d) × scale × dt × S` (force constante en norme ; `scale < 0` repousse).

### 8.10 `oscillatealpha` / `oscillatesize`

| Clé | `oscillatealpha` | `oscillatesize` |
|---|---|---|
| `frequencymin` | `0` | `0` |
| `frequencymax` | `10` | `10` |
| `scalemin` | `0` | `0.8` |
| `scalemax` | `1` | `1.2` |
| `phasemin` | `0` | `0` |
| `phasemax` | `2π` | `2π` |

Pour chaque particule (oscillateur correspondant) :
- **Première application** à cette particule (oscillateur non initialisé) : tirer, dans l'ordre,
  `f = U(frequencymin, frequencymax)`, une échelle `U(scalemin, scalemax)` (mémorisée mais **inutilisée**),
  `φ = U(phasemin, phasemax + 2π)` (noter le `+ 2π` sur la borne haute) ; `base = valeur courante` (alpha ou taille) ;
  marquer initialisé.
- Ensuite : `c = (cos(f × âge + φ) + 1) / 2` ; `m = lerp(scalemin, scalemax, c)` (avec les scalemin/scalemax
  **courants**, communs à toutes les particules) ; `valeur = base × m`.

Interaction avec l'ordre : `alphafade`/`alphachange` (resp. `sizechange`) mettent à jour `base` à chaque frame. Si
l'oscillateur est placé **après** eux, l'oscillation module la valeur fondue ; s'il est placé **avant**, l'opérateur
suivant écrase la valeur (l'oscillation n'est pas visible). Sans opérateur de fondu, `base` reste la valeur capturée
au premier passage.

### 8.11 `oscillateposition`

`frequencymin` = `0`, `frequencymax` = `5`, `scalemin` = `0`, `scalemax` = `10`, `phasemin` = `0`,
`phasemax` = `2π`, `mask` = `(1, 1, 0)`.

- Première application : pour chaque axe dans l'ordre x, y, z, tirer `f_a = U(fmin, fmax)`, `A_a = U(smin, smax)`,
  `φ_a = U(phasemin, phasemax + 2π)` (tirages entrelacés par axe : 9 tirages).
- Chaque frame : `position_a += −A_a × f_a × sin(f_a × âge + φ_a) × dt × mask_a × S` pour chaque axe.
  (C'est l'intégration d'Euler de la dérivée de `A cos(f t + φ)` : amplitude ≈ `A` pixels autour de la trajectoire.)

---

## 9. Fonctions mathématiques communes

### 9.1 Tirage uniforme `U(a, b)`

Flottant 32 bits uniforme dans `[a, b)`. Si `b < a`, les bornes sont échangées avant le tirage. Si `a == b`, renvoie
`a`. Vecteurs : un tirage indépendant par composante, dans l'ordre x, y, z.

### 9.2 Interpolation et rampe

Voir §8.1. `lerp` non borné (pas de saturation en dehors de la rampe).

### 9.3 Bruit de Perlin 3D (« improved noise », Ken Perlin 2002)

Évalué en **double précision** (entrées `f32` promues en `f64`, résultat reconverti en `f32`).

- Table de permutation `P[0..255]` = permutation de référence de Ken Perlin, répétée pour former 512 entrées
  (`P[i + 256] = P[i]`) :

```
151, 160, 137, 91, 90, 15, 131, 13, 201, 95, 96, 53, 194, 233, 7, 225,
140, 36, 103, 30, 69, 142, 8, 99, 37, 240, 21, 10, 23, 190, 6, 148,
247, 120, 234, 75, 0, 26, 197, 62, 94, 252, 219, 203, 117, 35, 11, 32,
57, 177, 33, 88, 237, 149, 56, 87, 174, 20, 125, 136, 171, 168, 68, 175,
74, 165, 71, 134, 139, 48, 27, 166, 77, 146, 158, 231, 83, 111, 229, 122,
60, 211, 133, 230, 220, 105, 92, 41, 55, 46, 245, 40, 244, 102, 143, 54,
65, 25, 63, 161, 1, 216, 80, 73, 209, 76, 132, 187, 208, 89, 18, 169,
200, 196, 135, 130, 116, 188, 159, 86, 164, 100, 109, 198, 173, 186, 3, 64,
52, 217, 226, 250, 124, 123, 5, 202, 38, 147, 118, 126, 255, 82, 85, 212,
207, 206, 59, 227, 47, 16, 58, 17, 182, 189, 28, 42, 223, 183, 170, 213,
119, 248, 152, 2, 44, 154, 163, 70, 221, 153, 101, 155, 167, 43, 172, 9,
129, 22, 39, 253, 19, 98, 108, 110, 79, 113, 224, 232, 178, 185, 112, 104,
218, 246, 97, 228, 251, 34, 242, 193, 238, 210, 144, 12, 191, 179, 162, 241,
81, 51, 145, 235, 249, 14, 239, 107, 49, 192, 214, 31, 181, 199, 106, 157,
184, 84, 204, 176, 115, 121, 50, 45, 127, 4, 150, 254, 138, 236, 205, 93,
222, 114, 67, 29, 24, 72, 243, 141, 128, 195, 78, 66, 215, 61, 156, 180
```

- Cellule : `X = ⌊x⌋ & 255` (idem Y, Z) ; partie fractionnaire `x − ⌊x⌋` (idem y, z).
- Fondu : `fade(t) = 6t⁵ − 15t⁴ + 10t³` appliqué aux trois fractions → `u, v, w`.
- Hachage des 8 coins (algorithme de référence) : `A = P[X] + Y`, `AA = P[A] + Z`, `AB = P[A+1] + Z`,
  `B = P[X+1] + Y`, `BA = P[B] + Z`, `BB = P[B+1] + Z` ; coins `P[AA]`, `P[BA]`, `P[AB]`, `P[BB]`, `P[AA+1]`,
  `P[BA+1]`, `P[AB+1]`, `P[BB+1]` pour les sommets `(0,0,0)`, `(1,0,0)`, `(0,1,0)`, `(1,1,0)`, `(0,0,1)`, `(1,0,1)`,
  `(0,1,1)`, `(1,1,1)`.
- Gradient : pour un hachage `h` (4 bits bas) et le vecteur relatif `(x, y, z)` = fractions moins les coordonnées
  du sommet (ex. pour le sommet `(1,0,1)` : `(fx − 1, fy, fz − 1)`) :
  `a = x si h < 8 sinon y` ; `b = y si h < 4, sinon x si h ∈ {12, 14}, sinon z` ;
  résultat `= (±a) + (±b)` avec `a` négatif si le bit 0 de `h` est levé, `b` négatif si le bit 1 est levé.
- Résultat : interpolation trilinéaire des 8 produits avec `u` (sur x), puis `v` (sur y), puis `w` (sur z).

**Bruit vectoriel** `N(p)` (vec3) : trois évaluations scalaires,
`N(p) = ( n(p), n(p + (89.2, 33.1, 57.3)), n(p + (100.3, 120.1, 142.2)) )` (décalages ajoutés en `f64`).

### 9.4 Bruit de curl

Différences centrées avec `ε = 1e−4` (en `f32`, donc imprécis pour de grandes coordonnées) :
`∂N/∂x ≈ (N(p + ε eₓ) − N(p − ε eₓ)) / (2ε)`, idem y, z.

```
curl(p) = ( ∂N_z/∂y − ∂N_y/∂z ,  ∂N_x/∂z − ∂N_z/∂x ,  ∂N_y/∂x − ∂N_x/∂y )
```

(6 évaluations du bruit vectoriel, soit 18 bruits scalaires par appel.)

### 9.5 Spline de Catmull-Rom uniforme

Pour 4 points `P0..P3` et `t ∈ [0, 1)` :

```
C(t) = ½ · [ 2P1 + (−P0 + P2) t + (2P0 − 5P1 + 4P2 − P3) t² + (−P0 + 3P1 − 3P2 + P3) t³ ]
```

---

## 10. Animation de sprite sheet

Exécutée après les opérateurs, seulement si `F > 0`. `vitesseAnim = sequencemultiplier` s'il est `> 0`, sinon `1`.

| `animationmode` | Calcul de l'image `frame` (flottant) |
|---|---|
| `"randomframe"` | si `frame < 0` (nouvelle particule) : entier uniforme dans `[0, F − 1]`, choisi **une fois** ; voir note |
| `"once"` | `frame = min(life × F × vitesseAnim, F − 1)` (non arrondi) |
| toute autre valeur (dont `"sequence"`) | si `T_anim > 0` : `frame = fmod( fmod(âge × vitesseAnim, T_anim) / T_anim × F , F )` ; sinon `frame = fmod(life × F × vitesseAnim, F)` |

Note `randomframe` : l'implémentation de référence tire cette image avec un générateur **local** dont la graine
dépend de l'emplacement mémoire de la particule dans le pool, et non du générateur du système. Effet observable :
la valeur n'est pas indépendante d'une apparition à l'autre — un même emplacement du pool produit toujours la même
image pendant une exécution. Recommandation : graine = fonction de hachage de l'indice d'emplacement
(voir Questions ouvertes).

Le passage de `frame` au shader est décrit au §12.3.

---

## 11. Mort

Une particule meurt uniquement par expiration (`âge ≥ duréeDeVie`), testée **après** les opérateurs ; elle n'est
donc jamais dessinée dans l'état expiré. Aucune mort liée à la taille, l'alpha ou la sortie d'écran. La
compaction est stable (l'ordre d'apparition est conservé), ce qui garantit que l'indice 0 est toujours la particule
la plus ancienne — propriété dont dépendent les ropes.

---

## 12. Rendu

### 12.1 Matériau, shader, combos, état GPU

- Matériau : fichier `.json` désigné par `material` (format matériau WE, cf. spec matériaux). Seule la **première
  passe** est utilisée.
- Shader : celui de la passe (typiquement `genericparticle`), remplacé par **`genericropeparticle`** pour `rope` et
  `ropetrail`.
- Combos imposés en plus de ceux de la passe : `THICKFORMAT = 1` (toujours) ; `SPRITESHEET = 1` si `F > 0` ;
  `TRAILRENDERER = 1` pour `spritetrail` et `ropetrail`.
- Texture : la texture principale est la **première** entrée (plus petit indice) de la table `textures` de la passe ;
  un nom commençant par `_rt_` ou `_alias_` désigne une cible de rendu de la scène, sinon c'est un asset texture.
  L'unité de texture 0 (`g_Texture0`) est **forcée** sur cette texture, même si l'annotation du shader propose une
  texture par défaut (ex. blanc).
- Mélange (clé `blending` de la passe, défaut `"normal"`, valeur inconnue → normal) :
  - `normal` : `src × 1 + dst × 0` (écrasement, couleur et alpha) ;
  - `translucent` : `src × αs + dst × (1 − αs)` (couleur et alpha) ;
  - `additive` : `src × αs + dst × 1` (couleur et alpha).
- `depthtest` (défaut désactivé), `depthwrite` (défaut désactivé), `cullmode` (défaut `nocull`) : selon la passe.
- **Depth clamp** activé pendant le dessin des particules (évite le clipping near/far des sommets légèrement
  décalés en Z par le shader) ; désactivé après. Sur une API sans depth clamp, désactiver le clipping de profondeur
  par un moyen équivalent (ex. « unclipped depth »).
- Cible : le framebuffer de la scène, viewport = taille complète de ce framebuffer.
- Dessin : triangles indexés (indices 32 bits), tampons de sommets/indices ré-uploadés intégralement chaque frame.
- Ordre : ordre du pool (plus ancienne d'abord, donc les plus récentes par-dessus). **Aucun tri** en profondeur.
- Constante de matériau `ui_editor_properties_overbright` (dans `constantshadervalues` de la première passe) :
  fournie comme `g_Brightness` ; défaut `1`.

**Combo `REFRACT`** (si la passe le déclare avec une valeur non nulle) : avant chaque dessin, copier le contenu
courant du framebuffer de la scène (taille réelle, filtrage au plus proche) dans une texture distincte de même
taille (RGBA8, UV bornées) que le shader voit sous le nom `_rt_FullFrameBuffer` ; ceci évite de lire la cible en
cours d'écriture. L'uniforme `g_RefractAmount` est fourni avec la valeur `0.05`.

### 12.2 Uniformes spécifiques

En plus des uniformes communs à toute passe (cf. spec matériaux/shaders : `g_Time`, `g_Texture0..7`,
`g_Texture0Resolution`, `g_PointerPosition`, etc.) :

| Uniforme | Valeur |
|---|---|
| `g_ModelMatrix` | `M` |
| `g_ModelMatrixInverse` | `M⁻¹` |
| `g_ViewProjectionMatrix` | `VP` |
| `g_ModelViewProjectionMatrix` | `VP · M` |
| `g_ModelViewProjectionMatrixInverse` | `(VP · M)⁻¹` |
| `g_OrientationUp` | `(0, 1, 0)` |
| `g_OrientationRight` | `(1, 0, 0)` |
| `g_OrientationForward` | `(0, 0, 1)` |
| `g_ViewUp` | `(0, 1, 0)` |
| `g_ViewRight` | `(1, 0, 0)` |
| `g_EyePosition` | `(0, 0, 1000)` — y compris en 2D (un œil sur Z=0 rendrait les traînées invisibles) |
| `g_RenderVar0` | `(longueurTraînée, maxlength, minlength, 0)` (§4.2) |
| `g_RenderVar1` | si `F > 0` et `C > 0` et `R > 0` : `(1/C, 1/R, F, ratio)` avec `ratio = (H_tex / R) / (W_tex / C)` où `W_tex, H_tex` = dimensions de la texture GPU de l'atlas (avec padding) ; sinon `(0, 0, 0, h/w)` avec `w, h` = dimensions réelles de l'image (par frame pour une texture animée) ; `ratio = 1` si la largeur est nulle ou sans texture |
| `g_Brightness` | overbright (§12.1) |
| `g_Alpha`, `g_UserAlpha` | `override.alpha` |
| `g_Color` | `override.color` (vec3) |
| `g_Color4` | `override.color` converti en vec4 selon §1.1 : scalaire `c` → `(c,c,c,c)` (donc `(1,1,1,1)` sans bloc `instanceoverride`) ; vec3 → `(r,g,b,0)` (cas du défaut `(1,1,1)` dès que le bloc est présent, ou d'une chaîne `"r g b"`) |
| `g_RefractAmount` | `0.05` (si `REFRACT`) |

Ces valeurs sont recalculées à chaque frame dessinée.

### 12.3 Géométrie « sprite » (sprite et spritetrail)

Par particule vivante, **sauf** si sa position contient une valeur non finie, ou si sa taille est non finie,
`≤ 0` ou `> 10000` (la particule est alors simplement omise du dessin) :

**Attribut de vie** `L` :
- par défaut `L = life` (âge / durée de vie) ;
- si `F > 0` et `frame ≥ 0` : `L = (frame + 0.5) / F` en mode `randomframe`, sinon `L = frame / F`.
  (Le shader de sprite sheet reconstruit l'image par `⌊fract(L) × F⌋` et le facteur de fondu vers l'image suivante
  par `fract(L × F)`.)

**4 sommets** identiques sauf l'UV, positions **au centre de la particule** en espace local (l'expansion en quad,
la rotation et l'orientation caméra sont faites par le vertex shader WE) :

| Sommet | UV |
|---|---|
| 0 | `(0, 1)` |
| 1 | `(1, 1)` |
| 2 | `(1, 0)` |
| 3 | `(0, 0)` |

Indices : `0, 1, 2, 2, 3, 0` (décalés de 4 par particule).

**Format de sommet** : 17 flottants, pas de 68 octets :

| Attribut shader | Composantes | Décalage (floats) | Contenu |
|---|---|---|---|
| `a_Position` | 3 | 0 | position locale x, y, z |
| `a_TexCoordVec4` | 4 | 3 | u, v, rotation.z, taille |
| `a_Color` | 4 | 7 | r, g, b, alpha |
| `a_TexCoordVec4C1` | 4 | 11 | vitesse x, y, z (espace local, px/s), `L` |
| `a_TexCoordC2` | 2 | 15 | rotation.x, rotation.y |

Un attribut absent du programme shader est simplement non lié.

**`spritetrail`** : même géométrie ; c'est le shader (combo `TRAILRENDERER`) qui étire le quad selon la vitesse en
utilisant `g_RenderVar0` (longueur, max, min).

### 12.4 Géométrie « rope » (rope et ropetrail)

Nécessite `n ≥ 2` particules ; sinon rien n'est dessiné. Toutes les particules du système (dans l'ordre
d'apparition, plus ancienne d'abord) sont reliées en **un seul** ruban. Pour `ropetrail`, l'historique de positions
par particule (`segments`) n'est **pas** implémenté : le comportement est celui de `rope` (avec `TRAILRENDERER` et la
subdivision par défaut 1). Aucune particule n'est filtrée (pas de test de valeurs non finies).

**Étape 1 — échantillonnage de la spline.** `σ = max(1, subdivision)`. Nombre de points `M = (n − 1) × σ + 1`.
Pour chaque segment `i ∈ [0, n−2]` entre les particules `i` et `i+1`, avec `P0 = part[i−1]` (ou `part[i]` si
`i = 0`), `P1 = part[i]`, `P2 = part[i+1]`, `P3 = part[i+2]` (ou `part[i+1]` pour le dernier segment), et pour
`k ∈ [0, σ)`, `t = k/σ` :
- position `= C(t)` (Catmull-Rom, §9.5) ;
- taille `= lerp(taille_i, taille_{i+1}, t)` ;
- couleur RGBA `= lerp((couleur_i, alpha_i), (couleur_{i+1}, alpha_{i+1}), t)`.
Le dernier point `M − 1` est exactement la dernière particule.

**Étape 2 — paramétrage UV.** `Q = M − 1` (nombre de quads).
- `uvs = uvscale` s'il est `> 0`, sinon `1`.
- `longueur = Q / uvs + 1` ; `utile = longueur − 1`.
- Lissage actif ssi `uvsmoothing` **et** système marqué « durées de vie uniformes » (§7.4) **et non** `uvscrolling` :
  calculer la longueur d'arc cumulée `A_j` entre points successifs (`A_0 = 0`) et `A_tot = A_{M−1}`.
- Défilement : si `uvscrolling` et `utile > 0` : `δ = fmod(t_g, 10000) × utile`, sinon `δ = 0`.
- Pour le quad `s` : `pos_traînée = A_s / A_tot × Q` si lissage actif et `A_tot > 0`, sinon `s` ; puis `+ δ`.
  (Le shader calcule la coordonnée V comme `pos_traînée / (longueur − 1)` ; `uvscale > 1` fait répéter la texture.)

**Étape 3 — quads.** Pour chaque `s ∈ [0, Q)` : début `= point[s]`, fin `= point[s+1]`, précédent
`= point[s−1]` (ou début si `s = 0`), suivant `= point[s+2]` (ou fin si hors limites). 4 sommets d'UV
`(0,0)`, `(1,0)`, `(1,1)`, `(0,1)` ; indices `0, 1, 2, 2, 3, 0`.

**Format de sommet** : 26 flottants, pas de 104 octets, identique pour les 4 sommets du quad sauf les UV :

| Attribut shader | Composantes | Décalage | Contenu |
|---|---|---|---|
| `a_PositionVec4` | 4 | 0 | début.xyz, taille au début |
| `a_TexCoordVec4` | 4 | 4 | fin.xyz, `longueur` |
| `a_TexCoordVec4C1` | 4 | 8 | précédent.xyz, `pos_traînée` |
| `a_TexCoordVec4C2` | 4 | 12 | suivant.xyz, taille à la fin |
| `a_TexCoordVec4C3` | 4 | 16 | couleur RGBA à la fin |
| `a_TexCoordC4` | 2 | 20 | u, v |
| `a_Color` | 4 | 22 | couleur RGBA au début |

L'élargissement du ruban (perpendiculaire à la tangente, selon l'œil `g_EyePosition`) est fait par le shader WE.

### 12.5 Capacité des tampons

- Sprites : `N_pool × 4` sommets, `N_pool × 6` indices.
- Ropes : `max(1, N_pool − 1) × σ × 4` sommets, `× 6` indices.

---

## 13. Control points liés à la souris

À chaque frame simulée (étape 5), pour chaque emplacement lié à la souris (bit 1 de `flags`) :

```
(sx, sy) = position normalisée de la souris dans l'espace de la scène, ∈ [0,1]²,
             origine en haut à gauche, y vers le bas (cas standard ; cf. spec entrées/scène :
             la position fenêtre est bornée au viewport puis projetée dans la zone UV visible de la scène
             selon le mode d'ajustement — fill/fit/stretch)
position = ( sx × W − W/2 ,  H/2 − sy × H ,  0 ) + offset − O
```

Le résultat est exprimé en espace local par simple soustraction de `O` : l'échelle, la rotation et la parallaxe du
système ne sont **pas** compensées. La souris est mise à jour par la scène avant le rendu des objets de la frame.
Les control points ne bougent pas pendant les frames non simulées.

Utilisateurs des control points : origine des émetteurs (§6.1), `mapsequencearoundcontrolpoint`, `vortex`,
`controlpointattract`.

---

## 14. Particules enfants (`children`)

Les entrées sont lues mais **aucun système enfant n'est instancié ni dessiné**. Clés lues (pour compatibilité de
parsing) :

| Clé | Type | Défaut |
|---|---|---|
| `type` | chaîne | `"static"` |
| `name` | chaîne | `""` |
| `particle` | chaîne (chemin) | `""` |
| `maxcount` | entier | `20` |
| `controlpointstartindex` | entier | `0` |
| `probability` | nombre | `1` |
| `angles` | vec3 [B] | `(0,0,0)` |
| `origin` | vec3 [B] | `(0,0,0)` |
| `scale` | vec3 [B] | `(1,1,1)` |

Pour reproduire 1:1, un implémenteur doit **ignorer** les enfants (voir Questions ouvertes pour une implémentation
future).

---

## 15. Récapitulatif des cas limites

- `maxcount × count` tronqué à 0 → pool de 1000.
- Pool plein → émetteurs gelés (minuteries comprises), pas de rattrapage ensuite.
- `instantaneous` plus grand que la place libre → excédent perdu.
- Émetteur boîte avec `flags & 2` : excédent d'accumulateur jeté (pas reporté).
- Sphère : la rafale instantanée **remplace** l'émission au taux de la première frame.
- Aucun opérateur `movement` → particules immobiles malgré leur vitesse.
- `lifetime ≤ 0` → jamais dessinée.
- Taille hors `(0, 10000]` ou position non finie → omise du dessin (sprites uniquement), mais simulée.
- Valeur [U] fournie sous forme de tableau JSON → nulle.
- `turbulentvelocityrandom.scale = 0` → aucune contrainte de cône.
- `vortex` avec `audioprocessingmode > 0` → inactif.
- `mapsequencearoundcontrolpoint.count ≤ 0` → indéfini (recommandation : 1).
- `parallaxDepth` nul → profondeur effective ±0,65.
- `dt` borné à 0,1 s ; `dt ≤ 0` → pas de simulation mais dessin.
- `visible = false` → simulation gelée (pas de vieillissement).
- Rope avec moins de 2 particules → rien.
- `g_Color4.a = 0` lorsque `instanceoverride` est présent.

---

## 16. Questions ouvertes

### 16.1 Comportements de l'amont probablement divergents de WE (à confirmer sur WE réel)

1. **Asymétries d'inversion Y** (§2.2) : `directions` inversé pour la boîte mais pas pour la sphère ; offsets de
   control points non inversés alors que l'origine d'émetteur l'est. Reproduire tel quel, mais à valider.
2. **Décalage implicite par CP0** : tout émetteur (défaut `controlpoint = 0`) est translaté par la position du
   control point 0. Correct dans WE ?
3. **`sizerandom` divise par 2** mais pas la taille de base (20) : sémantique rayon/diamètre incohérente.
4. **`rotationrandom` multiplié par `override.speed`.**
5. **Oscillateurs** : borne de phase `phasemax + 2π` ; échelle par particule tirée mais inutilisée ;
   la fréquence est utilisée comme pulsation (rad/s) et non en Hz.
6. **Vortex** : `+0.1` dans la largeur de transition ; traction de l'anneau croissante avec la distance.
7. **`turbulence`** : phase et vitesse tirées une seule fois pour tout le système (pas par particule) ; paramètres
   [U] correspondants non dynamiques.
8. **`randomframe`** : graine dépendant de l'adresse mémoire (non reproductible) ; la sémantique attendue est
   probablement « image aléatoire indépendante par particule ». Quelle option retenir pour Lumen ?
9. **Double application d'alpha** possible : `override.alpha` multiplie l'alpha des particules **et** est envoyé en
   `g_Alpha`/`g_UserAlpha` ; dépend de l'usage dans le shader WE.
10. **`g_Color4.a = 0`** quand `instanceoverride` est présent (conversion vec3→vec4) : probablement involontaire.
11. **Parallaxe** : profondeur minimale 0,65, et largeur utilisée pour les deux axes.
12. **Transformation parente** ignorée (objets enfants d'un groupe).
13. **Control points world space / souris** : pas de compensation de l'échelle, de la rotation ni de la parallaxe.
14. **Perspective** : œil fixe à `(0,0,1000)` indépendant de la caméra de la scène.
15. **`g_RefractAmount`** forcé à 0,05 : une valeur fournie par le matériau doit-elle primer ?
16. **Première frame** traitée par la valeur exacte `t_ref == 0`.
17. **Limite `dt ≤ 0.1`** : WE fait-il de même (ralenti sous 10 FPS) ?
18. **Expansion des sommets** : l'amont dépend entièrement des vertex shaders WE `genericparticle` /
    `genericropeparticle` (format « THICKFORMAT »). Si Lumen doit fournir ses propres shaders (ex. WGSL natif), la
    formule d'expansion du quad (position + offsets selon UV, taille, rotation, vecteurs `g_Orientation*`/`g_View*`)
    et du ruban doit être spécifiée séparément (spec shaders).

### 16.2 Paramètres lus mais ignorés par l'amont

`starttime` (pré-simulation), `instanceoverride.enabled`, `controlpoint.locktopointer`, émetteur `cone`, `id`,
`sign`/`speedmin`/`speedmax` pour `boxrandom`, `delay`/`duration`/périodicité pour `sphererandom`, tous les
`audioprocessing*` (émetteurs, `turbulence`, `vortex` sauf la désactivation), renderer `fadealpha`/`fadesize`/
`segments`, bits de `flags` du système autres que 4, bits de `flags` d'émetteur autres que 2 et 4,
`children` (entièrement).

### 16.3 Types WE connus non implémentés par l'amont

Noms JSON présumés (d'après la documentation publique de l'éditeur WE ; orthographe exacte à vérifier sur des
fichiers réels). Comportement amont : entrée ignorée.

- **Initialiseurs** : `positionoffsetrandom`, `mapsequencebetweencontrolpoints`, `inheritcontrolpointvelocity`,
  `remapinitialvalue`, `inheritinitialvaluefromevent`, éventuelle liste de couleurs (`colorlist`).
- **Opérateurs** : `capvelocity`, `boids`, `reducemovementnearcontrolpoint`, `remapvalue`,
  `inheritvaluefromevent`, `maintaindistancetocontrolpoint` (présumé), familles de collision
  (`collisionplane`, `collisionsphere`, `collisionbox`, `collisionquad`, `collisionbounds`, `collisionmodel`).
- **Émetteurs** : aucun autre type connu avec certitude au-delà de `boxrandom` / `sphererandom` (à vérifier :
  variantes audio-réactives ou par control points).
- **Renderers** : `ropetrail` n'est que partiellement implémenté (pas d'historique par particule) ; autres
  renderers éventuels inconnus.
- **Enfants** : types `static`, `eventfollow`, `eventspawn`, `eventdeath` (sémantique WE présumée : système enfant
  statique attaché au parent, ou instancié sur évènement de suivi/apparition/mort d'une particule parente avec
  transmission de control points à partir de `controlpointstartindex`, filtré par `probability`, borné par
  `maxcount`) — non spécifiable sans observation de WE.
- **Pré-simulation** (`starttime`) et **audio-réactivité** (spectre audio sur émetteurs/opérateurs).
