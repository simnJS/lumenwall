# Spec 05 — Système de particules

Statut : rédigée en clean-room à partir de l'observation du comportement de l'implémentation de référence
(révision amont `a0ddce5`, 2026-10-29). Ce document est **autosuffisant** : un implémenteur ne doit pas avoir besoin
d'autre source que les specs voisines (scène/caméra, matériaux/shaders, textures, temps global) et les fichiers
d'assets Wallpaper Engine (WE) eux-mêmes.

> **Amendements 2026-11-03** (fin de fichier) : ils **priment** sur le corps du texte en cas de conflit
> (pré-simulation `starttime`, `instanceoverride.brightness`, calcul du pool, oscillateurs,
> `mapsequencearoundcontrolpoint`, types et clés WE absents de la première rédaction).

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
| `starttime` | nombre [B] (s) | `0` | pré-simulation : voir **A1** (l'amont le lit en entier et l'ignore) |
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

---

## Amendements 2026-11-03

Réponses aux questions de l'implémenteur. Application de `docs/PARITY-POLICY.md` : règle 3 (parité visible
avec l'amont) par défaut ; règle 4 (combler les manques par rapport au contrat **publiquement documenté** par WE)
pour tout ce que l'amont ignore. Chaque point donne (a) le comportement amont, (b) la documentation publique WE,
(c) la règle à implémenter.

Marquage des règles :
- **[Doc WE]** : sémantique tirée de la documentation publique (URL citée), formulée ici avec nos mots ;
- **[Inféré des données]** : déduit des fichiers d'assets WE (presets, scènes d'aperçu) ; constantes et formules
  exactes inconnues, choisies pour être raisonnables — **à calibrer sur WE réel** ;
- **[Amont]** : comportement observé de l'implémentation de référence.

Sources documentaires (consultées le 2026-11-03) :
- Général : <https://docs.wallpaperengine.io/en/scene/particles/component/general.html>
- Émetteurs : <https://docs.wallpaperengine.io/en/scene/particles/component/emitter.html>
- Initialiseurs : <https://docs.wallpaperengine.io/en/scene/particles/component/initializer.html>
- Opérateurs : <https://docs.wallpaperengine.io/en/scene/particles/component/operator.html>
- Renderers : <https://docs.wallpaperengine.io/en/scene/particles/component/renderer.html>
- Enfants : <https://docs.wallpaperengine.io/en/scene/particles/component/children.html>
- Points de contrôle : <https://docs.wallpaperengine.io/en/scene/particles/component/control_point.html>
- SceneScript : <https://docs.wallpaperengine.io/en/scene/scenescript/reference/class/IParticleSystem.html> et
  <https://docs.wallpaperengine.io/en/scene/scenescript/reference/class/IParticleSystemInstance.html>

Corpus de données : tous les fichiers de définition de particules du dossier `assets` d'une installation WE
(`assets/particles/`, `assets/presets/*/particles/`, `assets/scenes/particleelementpreviews/*/particles/`),
**dédupliqués par contenu : 209 définitions uniques**. Le dossier `assets/particles/` seul ne contient que 6
exemples ; l'essentiel est dans `presets/` et dans les 50 scènes d'aperçu « un type d'élément par scène »
(`particleelementpreviews/`). Rien n'a été copié dans le dépôt. **Rappel important** : l'éditeur WE n'écrit en
général que les valeurs différentes du défaut ; les défauts des types non implémentés par l'amont ne sont donc
**pas observables** et ceux donnés ci-dessous sont provisoires.

### A1. `starttime` — pré-simulation

**(a) Amont.** Lu comme entier non signé (un flottant est tronqué : `0.80000001` → 0) puis **jamais utilisé** :
aucune pré-simulation ; la première frame ne dessine rien (§5, étape 2).

**(b) WE public [Doc WE].** Page « General », réglage *Start time* : le système est pré-simulé pendant la durée
indiquée, en secondes, au moment de sa création, pour éviter un écran vide ; il démarre comme s'il tournait déjà
depuis cette durée. Le pas de simulation n'est pas documenté.
Données : valeurs observées sur 209 définitions — `0` ×144, `3` ×17, `1` ×13, `15` ×13, `2` ×11, `10` ×5, `5` ×4
(dont `ash`), `9` ×1, `0.80000001` ×1 → la valeur est un **flottant**.

**(c) Règle.**
1. Lecture : `starttime` nombre [B], en `f32`, secondes. Non-nombre, négatif, NaN → 0. Borne haute **60 s**
   (garde-fou anti-blocage, règle 2 de la politique de parité) [Inféré des données : maximum observé 15].
2. Moment : pendant la **première frame traitée** du système (§5 étape 2, donc première frame où il est
   initialisé **et** visible). Après `t_ref = t_g`, si `T0 = starttime > 0`, exécuter `N` pas de simulation
   (ci-dessous), **puis dessiner dans la même frame**. Si `T0 = 0`, comportement inchangé.
3. Pas : `h = 1 / fps_max` (plafond d'images par seconde de l'application, spec 04/07, défaut 30 → `h = 1/30 s`),
   borné à `[1/240, 0.1]`. `N = max(1, arrondi(T0 / h))`. Exemple `ash` à 30 FPS : `N = 150` pas de 1/30 s.
   Justification : c'est le pas qu'aurait eu le système s'il avait réellement tourné ; il garde cohérents les
   comportements dépendants de la cadence (bit 2 « ≤ 1 particule par frame », intégration d'Euler).
4. Pas `k = 1 … N` : exécuter les étapes 4 à 10 du §5 avec `dt = h` et, à la place de `t_g`, le temps virtuel
   `t_k = t_g − (N − k) × h` (vu par `turbulence`, `turbulentvelocityrandom`, `remapvalue`…). Le dernier pas voit
   `t_g` ; la frame réelle suivante enchaîne normalement avec `dt = t_g' − t_ref`. Les control points liés à la
   souris prennent la position courante de la souris à chaque pas.
5. Tout l'état évolue exactement comme en fonctionnement normal : minuteries d'émetteurs (`delay`, `duration`,
   périodicité), rafale `instantaneous` (émise au pas 1), compteurs de séquence, oscillateurs, morts et compaction,
   systèmes enfants (A6.5 : créés pendant la pré-simulation et simulés dans les pas restants), temps système
   (`particlesystemtime`, A6.3.1).
6. Pas de pré-simulation : aux frames suivantes, lors d'un changement de résolution, ni lors d'un `play()`
   SceneScript qui relance un système terminé [Inféré des données ; la doc ne parle que de la création].
7. Purement CPU : aucun uniforme (`g_Time`…) n'est modifié ; aucun dessin entre les pas.
8. Performance : la pré-simulation peut être faite au chargement de la scène plutôt que dans la première frame, à
   condition que l'état obtenu soit identique ; elle ne doit **pas** être étalée sur plusieurs images affichées.

### A2. `instanceoverride.brightness`

**(a) Amont.** Clé non lue : aucun effet. Seule la constante de matériau `ui_editor_properties_overbright` de la
première passe (défaut 1) est utilisée (§12.1, uniforme `g_Brightness`).

**(b) WE public.** `instanceoverride.brightness` **n'est pas documenté publiquement** : la page SceneScript
`IParticleSystemInstance` liste `alpha`, `size`, `count`, `speed`, `lifetime`, `rate`, `colorn`,
`controlpoint0…7`, pas `brightness`. La page « General » documente seulement *Overbright* (réglage de matériau qui
ajuste la luminosité du système). [Doc WE]
Données : 5 occurrences dans des scènes — `1.64` (×2, scène de test 3018340632, objets `ash`), `5.0` (×3,
preset `lightning`). Les shaders de particules livrés avec WE (`genericparticle`, `genericropeparticle`)
déclarent un uniforme `g_Overbright` lié à la constante de matériau `ui_editor_properties_overbright` (défaut 1,
plage 0–5) et multiplient par lui la couleur **RGB** finale du fragment (après éclairage/réfraction, avant cutout
et brouillard) ; l'alpha n'est pas touché.

**(c) Règle [Inféré des données].**
- `brightness` : scalaire [U], défaut `1` (bloc ou clé absents), relu à chaque frame dessinée.
- Valeur fournie au shader pour `g_Overbright` **et** pour `g_Brightness` : `overbright_matériau × brightness`.
- Effet : multiplication du RGB final uniquement ; ni l'alpha, ni la couleur des sommets (`colorn`, pas de
  saturation 0–1 côté sommets), ni la simulation ne sont affectés. Pas de borne (0 → noir).
- Si Lumen remplace les shaders WE par des shaders natifs, reproduire la même place de multiplication
  (après éclairage/réfraction, avant cutout et brouillard).

### A3. Taille du pool : `f32` ou `f64` ?

**(a) Amont.** `maxcount` lu comme entier non signé (flottant JSON tronqué). Le multiplicateur `count` est stocké
en `f32` (valeur JSON arrondie au `f32` le plus proche). Le produit est calculé **en `f32`** : `maxcount` converti
en `f32`, multiplication `f32` arrondie au plus proche, puis troncature vers zéro ; 0 → 1000.
Exemple de la scène de test : `count = 0.63999999` → `f32` = 0,63999998569… ; 150 × cette valeur = 95,99999785
en exact, arrondi `f32` → **96,0** → **96**. Un calcul en `f64` (ou en `f32` élargi en `f64`) donnerait **95**.
Autre cas discriminant : `300 × 0.57` → 171 (`f32`) contre 170 (`f64`).

**(b) WE public.** *Max count* : nombre maximal de particules, détermine la mémoire allouée (page « General »).
La page SceneScript `IParticleSystemInstance` décrit `count` comme un facteur sur le **débit d'émission** et `rate`
comme un facteur sur la **cadence de simulation**, ce qui ne correspond pas à l'usage amont (`count` → pool,
`rate` → débit des émetteurs) ; aucune formule publiée → question ouverte A7.2. [Doc WE]

**(c) Règle [Amont, parité].**
```
N_pool = tronc( f32(maxcount) ×_f32 count_f32 )      // multiplication IEEE f32, arrondi au plus proche,
si N_pool ≤ 0 ou NaN : N_pool = 1000                 // sans FMA ni élargissement en f64
```
Vecteurs de test : (150, 0.63999999) → 96 ; (300, 0.57) → 171 ; (200, 0.7) → 140 ; (1000, 0.29) → 290 ;
(100, 0.3) → 30. Marquer `// parity:`.

### A4. Oscillateurs : application dès la frame d'initialisation

**(a) Amont.** Pour chaque particule, lors de la **première** exécution de l'opérateur qui la voit : tirage des
paramètres, capture de la base, **puis, dans la même passe**, calcul et écriture de la valeur modulée
(`oscillatealpha`, `oscillatesize`) ou du déplacement (`oscillateposition`). Les opérateurs tournant dans la
frame de naissance (§5, étapes 6 → 8), la **première image dessinée est déjà modulée**, avec `âge = dt`.

**(b) WE public.** La page « Operators » décrit des fréquence/échelle/phase tirées par particule ; rien sur
l'instant de première application. [Doc WE]

**(c) Règle.** Initialisation et première application **dans la même frame** (la frame de naissance) ; aucune
image non modulée. Précisions :
- l'état d'oscillateur est **un par type et par particule** (alpha, taille, position) : deux opérateurs du même
  type dans une liste partagent les mêmes tirages (le second ne retire pas) — cas absent des données ;
- les tirages d'initialisation ont lieu même si le facteur de blending d'opérateur (A6.1-a) vaut 0 ;
- `oscillateposition` : utiliser directement `f_a` comme pulsation (l'amont fait un aller-retour `×2π / 2π` en
  `f32`, écart ≤ 1 ulp, invisible).

### A5. `mapsequencearoundcontrolpoint` : calcul de θ

**(a) Amont.** `count` lu comme nombre [U] puis **tronqué vers zéro en entier** (`3.02` → 3, `2.2` → 2) ; compteur
entier `k`. `θ = (f32(k) / f32(count)) × 2π` : **division flottante `f32`**, puis multiplication par la constante
2π en `f32` (6,2831855), dans cet ordre ; puis `k = (k + 1) mod count` en entier. `count ≤ 0` : division par zéro.
Les clés `bounds`, `limitbehavior`, `axis` sont ignorées.

**(b) WE public [Doc WE]** (« Position around control point ») : *Count* = nombre de points de départ sur le cercle ;
*Bounds* = début/fin de l'arc dans [0, 1] (cercle complet = 0–1) ; *Orientation* repeat (boucle) / mirror
(aller-retour) ; *Axis* (surtout en 3D) ; *Speed min/max* ; *Control point* ; options *Restart with periodic
emission* et *Modify count with layer settings*.
Données (5 occurrences) : `count` 128, `3.02` (preset « trinity »), `2.2` (preset « dna », double hélice) — valeurs
de curseur dont la partie entière donne exactement le motif attendu, ce qui confirme un nombre **entier** de
points ; `bounds` `"0 1"`, `limitbehavior` `"repeat"`, `axis` `"0 1 0"`.

**(c) Règle.**
1. `n = tronc(count)` ; si `n ≤ 0` → `n = 1`.
2. Index `i` à partir du compteur persistant `k` :
   - `limitbehavior = "repeat"` (défaut) : `i = k mod n` ;
   - `"mirror"` : si `n ≥ 2`, `P = 2(n − 1)`, `m = k mod P`, `i = m` si `m < n`, sinon `P − m` ; si `n = 1`, `i = 0`.
   Puis `k = k + 1` (le stocker modulo `n` ou `P`).
3. `f = f32(i) / f32(n)` (**division `f32`**) ; `bounds = (b0, b1)` (vec2 [U], défaut `(0, 1)`) :
   `f' = b0 + f × (b1 − b0)` ; `θ = f' × 2π_f32`. Avec les défauts, résultat identique bit à bit à l'amont.
4. `axis` (vec3 [U], défaut `(0, 0, 1)` ; nul → `(0, 0, 1)`) : la rotation de l'étape 5 du §7.9 devient la rotation
   de `s` d'un angle **−θ** autour de `normalize(axis)` (formule de Rodrigues). Pour l'axe par défaut, c'est
   exactement la formule du §7.9.
5. *Restart with periodic emission* / *Modify count with layer settings* : bits de `flags` non identifiés dans les
   données → ignorés (A7.4).

### A6. Types et clés présents dans les données WE mais absents de la spec

#### A6.0 Inventaire (209 définitions uniques)

Déjà spécifiés, pour mémoire (occurrences) : émetteurs `sphererandom` 192, `boxrandom` 19 ; initialiseurs
`lifetimerandom` 209, `sizerandom` 208, `colorrandom` 187, `velocityrandom` 85, `rotationrandom` 58,
`alpharandom` 35, `turbulentvelocityrandom` 31, `angularvelocityrandom` 26, `mapsequencearoundcontrolpoint` 5 ;
opérateurs `movement` 196, `alphafade` 178, `sizechange` 68, `controlpointattract` 29, `angularmovement` 27,
`colorchange` 24, `oscillatealpha` 24, `turbulence` 18, `oscillateposition` 15, `alphachange` 8, `vortex` 7,
`oscillatesize` 5, `vortex_v2` 5 ; renderers `sprite` 135, `spritetrail` 33, `rope` 20, `ropetrail` 12.

Absents de la spec (par fréquence décroissante) :

| Nom JSON | Catégorie | Occurrences | Définitions | Section |
|---|---|---|---|---|
| `mapsequencebetweencontrolpoints` | initialiseur | 11 | 11 | A6.2.1 |
| `remapvalue` | opérateur | 10 | 8 | A6.3.1 |
| `reducemovementnearcontrolpoint` | opérateur | 9 | 5 | A6.3.2 |
| `positionoffsetrandom` | initialiseur | 5 | 5 | A6.2.2 |
| `maintaindistancebetweencontrolpoints` | opérateur | 5 | 5 | A6.3.3 |
| `hsvcolorrandom` | initialiseur | 4 | 4 | A6.2.3 |
| `remapinitialvalue` | initialiseur | 3 | 3 | A6.3.1 |
| `capvelocity` | opérateur | 3 | 3 | A6.3.4 |
| `boids` | opérateur | 3 | 3 | A6.3.5 |
| `maintaindistancetocontrolpoint` | opérateur | 3 | 3 | A6.3.6 |
| `inheritcontrolpointvelocity` | initialiseur | 1 | 1 | A6.2.4 |
| `colorlist` | initialiseur | 1 | 1 | A6.2.5 |
| `inheritinitialvaluefromevent` | initialiseur | 1 | 1 | A6.5 |
| `inheritvaluefromevent` | opérateur | 1 | 1 | A6.5 |
| `collisionplane`, `collisionsphere`, `collisionquad`, `collisionbounds`, `collisionmodel` | opérateurs | 1 chacun (aperçus seulement) | 1 | A6.3.7 |
| `layerimage` | émetteur | 1 | 1 | A6.4 |
| enfants `type` : absent (→ `static`) 31, `eventdeath` 17, `eventfollow` 12, `static` 5, `eventspawn` 4 | enfants | 69 | 50 | A6.5 |

Clés transverses absentes ou ignorées (A6.1) : blending d'opérateur `blendinstart`/`blendinend`/`blendoutstart`/
`blendoutend` (turbulence 4, oscillateurs 4, `capvelocity` 3, `remapvalue` 2, `controlpointattract` 1) ;
`exponent` sur `lifetimerandom` 10, `colorrandom` 7, `alpharandom` 6 ; `sphererandom` : `delay` 4, `duration` 32,
bit 4 de `flags` 5, bornes périodiques 5, `maxtoemitperperiod` 4 ; dans les scènes et presets (copies
comprises, non dédupliqués) : `instanceoverride.controlpoint1` 34, `controlpoint2` 22, `controlpointangle1` 9,
`controlpointangle2` 1, `brightness` 5 ; `dependencies` de l'objet de scène
(`emitterimage` 1, `collisionmodel` 1) ; `movement.flags` 3 ; renderer `sprite` `orientation`/`axis` 5.

#### A6.1 Mécanismes transverses

**a) Blending d'opérateur [Doc WE]** (page « Operators », section *Operator blending*) : tout opérateur peut
porter `blendinstart`, `blendinend`, `blendoutstart`, `blendoutend` (fractions de vie 0–1, [U] relus à chaque
frame) ; l'effet monte entre *blend-in start* et *blend-in end*, est plein ensuite, se défait entre *blend-out
start* et *blend-out end*. Amont : clés ignorées. Règle [Doc WE + Inféré pour la forme exacte] :
- `b_in = 1` si aucune clé `blendin*` n'est présente, sinon `rampe(life, blendinstart, blendinend, 0, 1)` (§8.1 ;
  clé manquante → 0) ;
- `b_out = 1` si aucune clé `blendout*` n'est présente, sinon `1 − rampe(life, blendoutstart, blendoutend, 0, 1)`
  (clé manquante → 1) ;
- `b = b_in × b_out`, par particule. L'opérateur calcule son nouvel état `X'` à partir de `X` (position, vitesse,
  rotation, vitesse angulaire, couleur, alpha, taille) puis on applique `X ← X + b × (X' − X)` à chaque champ
  qu'il modifie. Pour un opérateur incrémental (forces), cela revient à multiplier l'incrément par `b` ; pour un
  opérateur qui fixe une valeur, à interpoler. Les mises à jour de base d'oscillateur (§8.4–8.5) utilisent la
  valeur mélangée. Une suppression de particule (collision `delete`) n'a lieu que si `b > 0`.
- Sans clé de blending, `b = 1` : comportement strictement inchangé.

**b) `exponent` des initialiseurs aléatoires [Doc WE]** (page « Initializers » : biais du tirage, 1 = équilibré,
< 1 vers *max*, > 1 vers *min*). Amont : seuls `sizerandom` et `angularvelocityrandom` l'utilisent. Règle : pour
`lifetimerandom`, `alpharandom`, `colorrandom` (par canal, tirages indépendants), `velocityrandom` et
`rotationrandom` (par composante), clé `exponent` [U] défaut 1 : si `exponent = 1`, tirage `U(min, max)` inchangé ;
sinon `min + U(0,1)^exponent × (max − min)` avec `min`/`max` tels que lus (sans échange).

**c) `sphererandom` : délai, durée, périodicité, plafond par période [Doc WE]** (page « Emitters » : *Duration*,
*Delay*, *Random periodic emission* sont des paramètres communs sphère/boîte). Amont : ignorés pour la sphère.
Règle : appliquer à `sphererandom` les étapes 1 à 3 du §6.2 (délai, durée, périodicité, mêmes clés et défauts)
**avant** son étape 1 (§6.3) ; les frames où la boîte n'émettrait pas, la sphère n'accumule pas son taux. La
sémantique propre à la sphère pour `instantaneous` (remplacement) est conservée.
`maxtoemitperperiod` [Inféré des données] (entier ≥ 0, défaut 0 = illimité ; observé 8 et 32 sur des émetteurs
périodiques du preset `lightning`) : si le bit 4 est actif, un compteur est remis à 0 à chaque entrée en émission
et le nombre de particules émises pendant la fenêtre est plafonné à cette valeur (excédent jeté). S'applique aussi
à `boxrandom`. Exemple : `rate` 100, fenêtres de 1 s, plafond 32 → un éclair de 32 particules par seconde.

**d) `instanceoverride.controlpoint0…7` et `controlpointangle0…7` [Doc WE]** (SceneScript
`IParticleSystemInstance.controlpoint0…7` : position de chaque control point). Amont : ignorées. Données :
`controlpoint1` ×34, `controlpoint2` ×22 (chaînes `"x y z"` ou objets `{value, animation}`), `controlpointangle*`
×10. Règle : `controlpointN` (vec3 [U], relu à chaque frame simulée), s'il est présent, **remplace l'`offset`** du
control point N (§4.4) avec la même convention (pas d'inversion Y, bit world space respecté) ; un control point
non déclaré dans le fichier est alors traité comme déclaré avec `flags = 0` ; un point lié à la souris garde la
souris. Il est exposé en lecture/écriture aux scripts (`instance.controlpointN`). Les animations de timeline
(clé `animation`) sont hors périmètre : utiliser `value`. `controlpointangleN` : lu, sans effet (aucun type
spécifié n'utilise l'orientation d'un control point).

**e) Vitesse des control points** (nécessaire à A6.2.4) : pour chaque point, à chaque frame simulée après
l'étape 5 du §5, `v_cp = (position − position_précédente) / dt` ; 0 à la première frame et à la frame d'un
changement de résolution.

**f) `flags` du système (racine)** [Doc WE pour l'existence des options, Inféré pour les bits] : la page
« General » liste *Worldspace*, *Perspective rendering* et cinq bascules *Disable color / count / lifetime / size /
speed overrides*. Valeurs observées : 0, 1, 2, 3, 4, 6, 8, 9, 248. Bit 4 = perspective (§2.3, inchangé). Bits 8, 16,
32, 64, 128 = les cinq « disable overrides » (248 = les cinq, observé sur des systèmes enfants), ordre exact
inconnu. Règle provisoire : si `(flags & 248) == 248`, les multiplicateurs `colorn`, `count`, `lifetime`, `size`
et `speed` de l'instance valent 1 pour ce système ; combinaisons partielles ignorées. Bit 1 (probablement
*Worldspace* : les particules ne suivent plus la position/rotation du système après leur naissance) et bit 2 :
non spécifiés (A7.6).

**g) `dependencies` de l'objet de scène** [Inféré des données] : dans les scènes WE, un objet particules peut
porter `dependencies: [{ "id": <id d'objet>, "index": <entier>, "type": "emitterimage" | "collisionmodel" }]`,
qui relie un émetteur `layerimage` (A6.4) ou un opérateur `collisionmodel` à un autre objet. Voir A7.8 (la spec 02
attend des entiers).

#### A6.2 Initialiseurs

##### A6.2.1 `mapsequencebetweencontrolpoints` (11 occurrences)

[Doc WE] « Position between control points » : *Count* (nombre de points sur le segment), *Bounds* (début/fin
sur le segment, 0–1), *Orientation* repeat/mirror, *Control point start/end*, *Reduce outer positions /
velocities / sizes*, *Enable arc* (+ *Arc amount*, *Arc direction*), *Restart with periodic emission*.
Clés observées : `controlpointstart`, `controlpointend`, `count` (8, 10, 64), `limitbehavior` (`"mirror"`), `flags`
(3, 4, 7, 15, 19, 23), `arcamount` (0.1, 0.44).

| Clé | Type | Défaut | Origine du défaut |
|---|---|---|---|
| `controlpointstart` | entier [B] | `0` | presets sans la clé tracés depuis l'origine du système |
| `controlpointend` | entier [B] | `1` | idem, jusqu'au CP 1 déclaré |
| `count` | nombre [U], tronqué | `32` | [Inféré des données] l'éclair principal, sans `count`, émet 32 particules par période (`maxtoemitperperiod` 32) et son enfant `count` 8 pour 8 particules : un éclair = un parcours complet |
| `bounds` | vec2 [U] | `(0, 1)` | doc |
| `limitbehavior` | chaîne | `"repeat"` | doc |
| `arcamount` | [U] | `0.5` | provisoire (le preset `dischargearc` active l'arc sans `arcamount`) |
| `flags` | entier [B] | `0` | voir ci-dessous |

Bits de `flags` [Inféré des données, à valider] : **8 = arc activé** (forte présomption : seule différence entre
les presets `discharge` (7) et `dischargearc` (15)) ; **16 = redémarrage à chaque période d'émission** (présent
uniquement avec des émetteurs périodiques) ; **1, 2, 4 = les trois « reduce outer »**, affectation proposée :
1 = vitesses, 2 = tailles, 4 = positions (l'aperçu de `positionoffsetrandom` n'active que le bit 4).

Algorithme (état : compteur `k` persistant ; chaque particule mémorise son point d'apparition `E` du §6.1 et un
poids `w_seq`, défaut 1) :
1. `n = max(1, tronc(count))` ; index `i` comme A5 étape 2 (repeat/mirror) ; `k += 1`. Si le bit 16 est actif, `k`
   est remis à 0 à chaque entrée en émission de la fenêtre périodique de l'émetteur.
2. `u = i / (n − 1)` si `n ≥ 2`, sinon `0` (les deux extrémités sont atteintes) ; `s = b0 + u × (b1 − b0)`.
3. `A = CP[start].position`, `B = CP[end].position` (indice hors `[0, 8)` → `(0,0,0)`) ; `p = lerp(A, B, s)`.
4. Arc (bit 8) : `d = B − A` ; si `|d.xy| > 1e−4`, `⊥ = normalize((−d.y, d.x, 0))` ;
   `p += ⊥ × 2 × arcamount × |d| × s × (1 − s)` (flèche au milieu = `arcamount × |d| / 2`).
5. `w = sin(π × s)` (0 aux extrémités, 1 au milieu).
6. `position = p + (position − E) × (bit 4 ? w : 1)` (le décalage déjà donné par l'émetteur et les initialiseurs
   précédents, p. ex. `positionoffsetrandom`, est conservé, réduit près des extrémités si bit 4) ;
   `w_seq = bit 4 ? w : 1`.
7. Bit 1 : `vitesse *= w`. Bit 2 : `taille *= w` et taille initiale idem.

##### A6.2.2 `positionoffsetrandom` (5 occurrences)

[Doc WE] décale la position avec un bruit fractal ; paramètres *Directions*, *Sign* (0 = deux signes, 1 = positif,
−1 = négatif), *Distance* (décalage maximal), *Octaves*, *Noise scale*, *Noise speed*. Clés observées :
`distance` (100, 150, 200), `scale` (0 ×4), `timescale` (5, 10).

| Clé | Défaut (provisoire) |
|---|---|
| `distance` [U] | `50` |
| `scale` [U] (échelle spatiale du bruit, par pixel) | `0.01` |
| `timescale` [U] (vitesse du bruit) | `1` |
| `octaves` [U], tronqué, ≥ 1 | `3` |
| `directions` vec3 [B] | `(1, 1, 1)` |
| `sign` ivec3 [B] | `(0, 0, 0)` |

Algorithme : `q = position × scale + (t_g × timescale) × (1,1,1)` ;
`fbm(q) = Σ_{o=0}^{O−1} 0.5^o × N(2^o × q) / Σ_{o=0}^{O−1} 0.5^o` avec `N` le bruit vectoriel du §9.3 ;
`δ = fbm(q) × distance × directions` (composante par composante), signe appliqué comme pour la sphère (§6.3) ;
`position += δ × w_seq`. Avec `scale = 0` (cas des éclairs), le décalage ne dépend que de l'instant d'apparition :
des particules nées successivement forment une ligne brisée continue.

##### A6.2.3 `hsvcolorrandom` (4 occurrences)

[Doc WE] *Hue min/max*, *Hue steps* (nombre de teintes distinctes), *Saturation min/max* (0–1), *Value min/max*
(0–1). Clés : `huemin`, `huemax`, `huesteps`, `saturationmin`, `saturationmax`, `valuemin`, `valuemax`.
Défauts [Inféré] : `huemin` 0, `huemax` 1, `huesteps` 0 (continu), `saturationmax` 1, `valuemax` 1,
`saturationmin` 0, `valuemin` 0 (les données écrivent explicitement `saturationmin`/`valuemin` = 1, donc ce n'est
pas leur défaut).
Algorithme (tirages dans l'ordre h, s, v) :
- `huesteps ≥ 1` : `j = min(tronc(U(0,1) × huesteps), huesteps − 1)`,
  `h = huemin + (j / huesteps) × (huemax − huemin)` (6 pas sur 0–1 → rouge, jaune, vert, cyan, bleu, magenta) ;
  sinon `h = U(huemin, huemax)` ;
- `s = U(saturationmin, saturationmax)`, `v = U(valuemin, valuemax)` ;
- conversion HSV → RGB standard avec `h' = fract(h) × 6`, secteur `⌊h'⌋`, `f = h' − ⌊h'⌋`, `p = v(1 − s)`,
  `q = v(1 − s f)`, `t = v(1 − s(1 − f))` → secteurs 0…5 : `(v,t,p)`, `(q,v,p)`, `(p,v,t)`, `(p,q,v)`, `(t,p,v)`,
  `(v,p,q)` ;
- `couleur = rgb × colorn` ; met à jour la couleur initiale (comme §7.1).

##### A6.2.4 `inheritcontrolpointvelocity` (1 occurrence)

[Doc WE] ajoute à la particule la vitesse courante d'un control point, dans une plage de vitesse.
Clés : `controlpoint` (entier, défaut 0), `min` (défaut 1), `max` (défaut 1) — observé `controlpoint` 1, `min` 0.3,
`max` 1. Règle : `vitesse += v_cp(controlpoint) × U(min, max) × S` (`v_cp` : A6.1-e ; `S` = `override.speed`).

##### A6.2.5 `colorlist` (1 occurrence)

[Doc WE] choisit pour chaque particule une couleur dans une liste (≤ 10) ; bruits de teinte/saturation/valeur
optionnels. Clé `colors` : tableau de couleurs [C] (observé : chaînes flottantes `"r g b"`). Règle :
`m = len(colors)` ; si `m = 0`, sans effet ; sinon `j = min(tronc(U(0,1) × m), m − 1)`,
`couleur = colors[j] × colorn`, couleur initiale idem. Bruits HSV : noms de clés non observés → ignorés.

#### A6.3 Opérateurs

##### A6.3.1 `remapvalue` (10 occurrences / 8 définitions) et initialiseur `remapinitialvalue` (3)

[Doc WE] lit une entrée (attribut de particule, control point ou valeur globale : durée d'exécution, heure du
jour…), la normalise sur une plage d'entrée, la transforme (fonction d'onde ou bruit), la projette sur une plage de
sortie et l'applique à une propriété par *Assign*, *Multiply*, *Add* ou *Subtract* ; *Clamp input* / *Clamp output*
optionnels. L'initialiseur agit une fois à la naissance, l'opérateur à chaque frame.

| Clé | Type | Défaut |
|---|---|---|
| `input` | chaîne | `"lifetimefraction"` [Inféré : les remaps de pluie sans `input` modulent au fil de la vie] |
| `inputcontrolpoint0`, `inputcontrolpoint1` | entier [B] | `0`, `1` |
| `inputrangemin`, `inputrangemax` | [U] | `0`, `1` |
| `output` | chaîne | `"size"` [Inféré : remap de distance sans `output` sur un éclair = effilement ; exemple de la doc] |
| `outputrangemin`, `outputrangemax` | [U] scalaire ou vec3 (diffusé) | `0`, `1` |
| `operation` | chaîne | `"remap"` (= Assign) ; `"multiply"`, `"add"`, `"subtract"` |
| `transformfunction` | chaîne | `"none"` ; `"sine"`, `"square"`, `"saw"`, `"triangle"`, `"simplexnoise"`, `"fbmnoise"` |
| `transforminputscale` | [U] | `1` |
| `flags` | entier [B] | **`1`** : bit 1 = borner l'entrée, bit 2 = borner la sortie [Inféré : un preset écrit explicitement `0` pour une entrée temporelle non bornée, un autre `3`] |

Entrées scalaires supportées : `lifetimefraction` (`life`), `maxlifetime` (durée de vie, s), `size`, `opacity`,
`speed` (`|vitesse|`), `rotation` (`rotation.z`), `angularspeed` (`|vitesseAngulaire|`), `distancetocontrolpoint`
(`|position − CP[c0]|`), `positionbetweentwocontrolpoints` (`dot(p − A, B − A) / |B − A|²` avec `A = CP[c0]`,
`B = CP[c1]`, 0 si `A = B`), `runtime` (`t_g`), `particlesystemtime` et `layertime` (Σ des `dt` simulés depuis la
création du système, pré-simulation comprise), `timeofday` (secondes locales depuis minuit / 86400). Entrées
vectorielles ou inconnues (`position`, `velocity`, `color`, `controlpoint`, `deltatocontrolpoint`,
`directiontocontrolpoint`, `layerorigin`) : entrée inactive (ignorée) en attendant des données.

Calcul, par particule :
1. `x` = entrée ; `n = (x − inmin) / (inmax − inmin)` (si `inmax = inmin` : `n = x ≥ inmax ? 1 : 0`) ; bit 1 →
   `n = clamp(n, 0, 1)`.
2. Transformation (σ = `transforminputscale`) : `none` → `T = n` ; `sine` → `0.5 + 0.5 sin(2π σ n)` ; `square` →
   `fract(σ n) < 0.5 ? 1 : 0` ; `saw` → `fract(σ n)` ; `triangle` → `1 − |2 fract(σ n) − 1|` ; `simplexnoise` →
   `T = 0.5 + 0.5 × N((σ n, φ_p, 0))` (bruit vectoriel du §9.3, une composante par composante de sortie ;
   `φ_p = U(0, 256)` tiré une fois par particule à la première évaluation) ; `fbmnoise` → idem avec la somme
   fractale de A6.2.2 sur 4 octaves.
3. `y = outmin + T × (outmax − outmin)` (par composante) ; bit 2 → borner `y` à `[min(outmin,outmax),
   max(outmin,outmax)]`.
4. Application à la sortie `X` : `remap` → `X = y` ; `multiply` → `X × y` ; `add` → `X + y` ; `subtract` → `X − y`.
   Sorties : `size`, `opacity` (alpha), `color` (vec3), `velocity` (vec3, **même convention Y que
   `velocityrandom`**, §2.2), `speed` (norme de la vitesse, direction conservée ; sans effet si `|v| < 1e−6`),
   `rotation` (vec3), `angularspeed` (vitesse angulaire vec3), `position` (vec3, convention Y de l'origine
   d'émetteur). Sortie inconnue → entrée inactive.
5. Initialiseur : `X` = valeur courante dans la chaîne d'initialiseurs ; met aussi à jour les valeurs initiales
   (taille, alpha, couleur). Opérateur : pour `size`, `opacity`, `color` avec `multiply`/`add`/`subtract`, partir
   de la **valeur initiale** (pas de composition exponentielle d'une frame à l'autre) ; pour les autres sorties,
   de la valeur courante ; après écriture de l'alpha ou de la taille, mettre à jour la base de l'oscillateur
   correspondant (comme §8.4–8.5). Le blending (A6.1-a) s'applique à l'opérateur.

##### A6.3.2 `reducemovementnearcontrolpoint` (9 / 5)

[Doc WE] réduit la vitesse près d'un control point ; *Distance inner/outer*, *Reduction inner/outer* (négatif =
accélère). Clés : `controlpoint` (défaut 0), `distanceinner` (100), `distanceouter` (200), `reductioninner` (1),
`reductionouter` (0) — défauts provisoires ; observé 20/50/1000 (éclairs, une instance sur CP0 et une sur CP1)
et 50/150/10/0 (aperçu). Pour chaque particule : `d = |position − CP.position|` ;
`r = d ≤ din ? rin : (d ≥ dout ? rout : lerp(rin, rout, (d − din)/(dout − din)))` ;
`vitesse *= max(0, 1 − r × dt)` (même forme que la traînée de `movement` ; `r < 0` accélère). Indice hors
`[0, 8)` → inactif.

##### A6.3.3 `maintaindistancebetweencontrolpoints` (5)

[Doc WE] maintient la particule entre deux control points (la suit quand ils bougent). Aucune clé autre que `name`
observée ; clés supposées `controlpointstart` (0) et `controlpointend` (1). Règle : à la première application à
une particule, mémoriser `u_p` = paramètre `s` posé par `mapsequencebetweencontrolpoints` s'il existe, sinon
`clamp(dot(p − A, B − A)/|B − A|², 0, 1)` ; ensuite, à chaque frame,
`position += lerp(A_t, B_t, u_p) − lerp(A_{t−1}, B_{t−1}, u_p)` (positions des control points à cette frame et à
la précédente). Points immobiles → aucun effet visible.

##### A6.3.4 `capvelocity` (3)

[Doc WE] vitesse maximale par particule, combinable avec le blending. Clé `maxspeed` [U] (défaut provisoire 100 ;
observé 50 et 100). Si `|v| > maxspeed` : `v = v / |v| × maxspeed` ; puis blending (A6.1-a).

##### A6.3.5 `boids` (3)

[Doc WE] *Neighbor threshold* (rayon de voisinage pour alignement et cohésion), *Separation threshold* (distance
d'évitement), *Separation / Cohesion / Alignment factor*, *Clamp speed* + *Max speed*. Clés observées :
`neighborthreshold` (50, 150), `separationfactor` (0, 25), `cohesionfactor` (4, 5), `alignmentfactor` (5),
`flags` (0). Défauts provisoires : `neighborthreshold` 100, `separationthreshold` 25, facteurs 1, `maxspeed` 1000,
`flags` bit 1 = borner la vitesse.
Règle (deux passes : toutes les variations sont calculées sur l'état en début d'opérateur, puis appliquées) :
pour la particule i, voisins `V = { j ≠ i : |p_j − p_i| < neighborthreshold }` ; si `V` vide, rien ;
`c = moyenne(p_j) − p_i` ; `a = moyenne(v_j) − v_i` ;
`s = Σ_{j : |p_i − p_j| < separationthreshold, > 1e−4} (p_i − p_j)/|p_i − p_j| × (separationthreshold − |p_i − p_j|)` ;
`v_i += (cohesionfactor × c + alignmentfactor × a + separationfactor × s) × dt` ; si bit 1, borner `|v_i|` à
`maxspeed`. Performance (règle 1) : grille spatiale de pas `neighborthreshold`, pas de boucle O(n²).

##### A6.3.6 `maintaindistancetocontrolpoint` (3)

[Doc WE] place les particules à une distance donnée d'un control point, avec une force variable (0–1).
Clés : `controlpoint` (0), `distance` (provisoire 100), `variablestrength` (observé 5, hors de la plage documentée).
Règle : `δ = p − CP.position`, `d = |δ|` ; si `d > 1e−4` : `cible = CP.position + δ/d × distance`,
`α = clamp(variablestrength, 0, 1)`, `p = lerp(p, cible, α)`.

##### A6.3.7 Collisions (1 occurrence chacune, scènes d'aperçu uniquement — priorité basse)

[Doc WE] comportements *Bounce* (avec *Bounce factor*), *Slide*, *Stop*, *Delete* (supprime la particule et déclenche
l'évènement de mort) ; variantes plan, sphère, limites de la scène, rectangle (un seul côté), modèle.
Clés communes : `collisionbehavior` (`"bounce"` défaut, `"slide"`, `"stop"`, `"delete"`), `bouncefactor`
(défaut provisoire 0.5 ; observé 0.7). Toutes les grandeurs vectorielles (`plane`, `origin`, `forward`) suivent la
**même convention d'axe Y que `gravity`** (§2.2), de sorte qu'un sol placé sous l'émetteur arrête des particules
tombant par gravité.
Réponse à une pénétration (distance signée `σ < 0`, normale sortante `n`) : `position −= σ × n` ;
`vn = dot(v, n)` ; si `vn < 0` : bounce → `v −= (1 + bouncefactor) × vn × n` ; slide → `v −= vn × n` ;
stop → `v = 0` ; delete → la particule meurt à cette frame (âge = durée de vie).
- `collisionplane` : `plane` (normale, défaut `(0,1,0)`), `distance` (défaut 0) ; `σ = dot(p, n̂) − distance`.
- `collisionsphere` : `origin` (défaut 0), `radius` (provisoire 100) ; la particule reste **à l'extérieur** :
  `σ = |p − c| − radius`, `n = (p − c)/|p − c|`.
- `collisionbounds` : les quatre bords du rectangle de scène `[−W/2, W/2] × [−H/2, H/2]` exprimés en espace local
  par soustraction de `O` ; normales vers l'intérieur.
- `collisionquad` : `origin`, `size` (vec2, défaut `(100, 100)`), `plane` (normale, défaut `(0,1,0)`), `forward`
  (défaut `(0,0,1)`) ; collision seulement si la particule passe du côté positif au côté négatif pendant la frame
  (position précédente estimée `p − v dt`) et si sa projection tombe dans le rectangle (axes `normalize(cross(
  forward, n̂))` de demi-largeur `size.x/2` et `forward` de demi-largeur `size.y/2`, ce second test omis en 2D).
- `collisionmodel` : nécessite un modèle 3D / puppet warp relié par `dependencies` → **non supporté**, ignoré
  (journalisé).

##### A6.3.8 `inheritvaluefromevent` (1) : voir A6.5.

#### A6.4 Émetteur `layerimage` (1 occurrence)

[Doc WE] émet depuis un calque image, texte ou puppet warp ; options *Copy layer color*, *Update emission bitmap
periodically*, *Inherit layer motion*, *Random offset* ; un masque optionnel limite la zone ; les autres réglages
sont ceux de la sphère et de la boîte. Données : émetteur `{name: "layerimage", rate: 5000}` (aucune autre clé),
`maxcount` 10000, lien par `dependencies: [{id, index: 0, type: "emitterimage"}]` sur l'objet particules ; le calque
source est **invisible** (`visible: false`) et sert quand même de source.
Règle [Inféré des données] :
- Source : l'objet de scène dont l'`id` est celui de la dépendance `emitterimage` dont `index` = indice de
  l'émetteur dans `emitter` ; absent → l'émetteur n'émet rien (journalisé).
- Bitmap d'émission : canal alpha de la texture de base du calque, calculé **une fois** au chargement
  (réduction autorisée à ≤ 256×256 texels) ; texels retenus : alpha ≥ 0,5. Aucun texel → rien.
- Taux, `instantaneous`, `delay`, `duration`, périodicité, `flags` bit 2 : comme `boxrandom` (§6.2).
- Position : texel tiré uniformément parmi les texels retenus, point uniforme dans le texel, `(u, v)` projeté sur
  le quad du calque avec la même transformation que son rendu (origine, taille, échelle, angles ; spec 04), puis
  ramené en espace local du système par soustraction de `O` (échelle/rotation du système non compensées, comme les
  control points world space). Vitesse nulle, `z = 0` ; puis valeurs de base (§3) et initialiseurs.
- Options *copy layer color*, *inherit layer motion*, *random offset*, masque, mise à jour périodique : clés non
  observées → ignorées. Animation de puppet warp : non prise en compte (texture statique).

#### A6.5 Systèmes enfants (`children`) et héritage de valeurs d'évènement

[Doc WE] (page « Children ») : *Static* = un enfant créé une fois à l'origine du système ; *Event follow* = un
enfant par particule, qui la suit ; *Event spawn* = enfant créé à la naissance d'une particule ; *Event death* =
enfant créé à la mort d'une particule, à sa position. *Offset*, *Angles*, *Scale* ; *Max count* (nombre maximal
d'enfants, utile pour les évènements) ; *Probability* (0–1) ; *Set control points* : les control points de l'enfant
reçoivent à chaque frame des positions de particules du parent à partir de *Control point start index*.
Données : clés `type` (absent → `static`), **`name` = chemin du fichier de définition de l'enfant** (la clé
`particle` du §14 n'apparaît jamais), `maxcount`, `probability`, `origin`, `angles`, `scale`, `flags` (0, 1, 2),
`controlpointstartindex` (null ou 1). Les control points d'un enfant peuvent porter `parentcontrolpoint`
(*Copy from parent*).
Règle [Doc WE + Inféré des données] — remplace « ignorer » du §14 :
1. Instance enfant = système complet (§0–§13) chargé depuis `name`, avec son pool, son générateur aléatoire et ses
   control points ; il reçoit l'`instanceoverride` et la visibilité du parent (sauf bits A6.1-f de ses propres
   `flags`) ; il est dessiné juste après son parent, dans le même emplacement de l'ordre de rendu.
2. Repère : matrice modèle de l'enfant = `M_parent · T(ancre + origin) · Rz(−angles.z) · Ry(angles.y) ·
   Rx(−angles.x) · S(scale)` (même convention qu'au §2.3) ; `ancre = (0,0,0)` pour `static`, position (espace
   local du parent) de la particule déclencheuse pour les évènements.
3. `static` : une instance créée avec le parent, vivant aussi longtemps que lui (`maxcount` sans objet).
4. `eventspawn` : après les initialiseurs de chaque nouvelle particule parente, si `U(0,1) < probability`
   (générateur du parent) et si le nombre d'instances vivantes de cette entrée est `< maxcount`, créer une instance
   ancrée à la position de naissance (fixe). Sinon l'évènement est perdu.
5. `eventdeath` : idem au moment de la mort (§5 étape 10, avant compaction ; morts par collision `delete`
   comprises), ancre = dernière position.
6. `eventfollow` : idem à la naissance ; l'ancre suit ensuite la position courante de la particule ; à la mort de
   celle-ci, l'instance cesse d'émettre.
7. Fin d'une instance d'évènement : détruite dès qu'elle n'a plus de particule vivante **et** qu'aucun de ses
   émetteurs ne peut plus émettre (rafale faite et `rate = 0`, `duration` écoulée, ou parent suivi mort).
8. *Set control points* (supposé `flags & 1`) : à chaque frame, `CP_enfant[controlpointstartindex]` (null → 0) ←
   position de la particule déclencheuse/suivie, exprimée dans le repère de l'enfant ; pour `static`,
   `CP_enfant[k + i]` ← particule parente `i` pour `i < 8 − k`. `parentcontrolpoint` et les autres bits : A7.7.
9. Ordre : le parent est simulé (évènements compris), puis ses enfants dans l'ordre de création, avec le même
   `dt` (et les mêmes pas pendant la pré-simulation, A1).
10. Héritage d'évènement : `inheritinitialvaluefromevent` (initialiseur, à la naissance d'une particule de
    l'enfant) et `inheritvaluefromevent` (opérateur, chaque frame tant que la particule parente vit) copient une
    valeur de la particule parente associée à l'instance. Options (noms d'énumération de l'interface WE) :
    `setcolor`, `multiplycolor`, `setopacity`, `multiplyopacity`, `setcoloropacity`, `multiplycoloropacity`,
    `setvelocity`, `addvelocity`, `setsize`, `multiplysize`, `setrotation`, `addrotation`, `setangularvelocity`,
    `addangularvelocity`. Nom de clé non observé (les données n'ont que des défauts) : lire une chaîne dans `input`
    si présente, défaut **`setcolor`** (exemple de la doc : l'explosion hérite de la couleur de la fusée). Sans
    instance parente (système racine), sans effet.

#### A6.6 Autres clés observées, lues sans effet pour l'instant

- renderer `sprite` : `orientation` (`"screen"` ×5 = comportement actuel) et `axis` ; *upright* / *fixed* (doc
  « Renderers ») non spécifiés.
- `movement.flags` bit 1 (×3) : option *Worldspace* du mouvement (doc « Operators ») — non spécifiée.
- `ropetrail` : `segments` (×2), `fadealpha` (×2) — inchangé (§4.2).
- control points : `flags` 4 et 16 observés, `parentcontrolpoint` — sémantique non établie (A7.7).
- clés d'audio-réactivité (`audioprocessing*`) — inchangé (audio hors périmètre de cette spec).

### A7. Questions ouvertes ajoutées

1. **`starttime` et visibilité** : WE pré-simule-t-il un système masqué au chargement (ex. `ash`, visible lié à une
   propriété utilisateur fausse par défaut) ? Choix actuel : à la première frame visible.
2. **`instance.count` / `instance.rate`** : la doc SceneScript les décrit comme facteurs du débit d'émission et de
   la cadence de simulation, l'amont les applique au pool et au débit des émetteurs. À trancher sur WE réel (un
   `rate` qui ralentirait le temps de simulation changerait fortement le rendu de la scène de test).
3. **`brightness`** : confirmer qu'il multiplie l'overbright (et non la couleur des sommets).
4. **Bits de `flags`** de `mapsequencebetweencontrolpoints` / `mapsequencearoundcontrolpoint` (reduce outer ×3,
   restart périodique, modify count with layer settings) ; défaut de `count` (32) et d'`arcamount` ; clé d'*Arc
   direction*.
5. **Défauts non observables** de tous les types de A6 : à calibrer sur WE réel.
6. **`flags` du système** : bit 1 (*Worldspace*), bit 2, et répartition des bits 8–128 entre les cinq « disable
   overrides ».
7. **Control points** : la doc indique que le control point 0 est toujours lié à l'origine du système, alors que
   l'amont lui applique son `offset` (§4.4) ; sémantique des bits 4 et 16 et de `parentcontrolpoint`.
8. **Spec 02, `dependencies`** : la spec 02 (§7.2) exige des entiers, or les scènes WE contiennent des objets
   `{id, index, type}` (A6.1-g) ; avec la règle actuelle, ces scènes échoueraient (E1). À amender dans la spec 02
   (hors du périmètre d'écriture de ce document).
9. **Fréquence des oscillateurs** : la doc « Operators » exprime *Frequency min/max* en **nombre d'oscillations par
   durée de vie** de la particule, alors que l'amont l'utilise comme pulsation en rad/s sur l'âge (§2.1, §8.10).
   Si la règle 5 de la politique de parité est appliquée, la phase deviendrait `2π × f × life + φ` ; à confirmer
   avant de changer.
