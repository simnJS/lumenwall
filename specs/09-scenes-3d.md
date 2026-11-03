# Spec 09 — Scènes 3D : modèles statiques, caméra perspective, éclairage, ombres, réflexions

Statut : rédigée en clean-room le 2026-11-03. Aucune ligne de code amont n'est reproduite, ni aucun extrait de shader
WE : les formules sont réécrites en notation mathématique.
Public : implémenteurs Rust de `lumenwall-formats` (lecture `.mdl`), `lumenwall-scene` (objets modèle, lumière,
sprite, caméra), `lumenwall-render` (profondeur, passes 3D, réflexion, ombres), `lumenwall-shader` (dialecte,
module `LightingV1`, combos moteur).

Mots normatifs : **DOIT**, **NE DOIT PAS**, **DEVRAIT**, **PEUT**.

## Portée et motif

Plusieurs scènes installées n'affichent aujourd'hui que leurs effets 2D et leurs particules, sans leurs modèles 3D.
Cette spec décrit ce qu'il faut pour les dessiner comme Wallpaper Engine (WE) : objets modèle, format binaire des
`.mdl` statiques, caméra perspective et ses chemins, lumières, profondeur, réflexion plane, ombres, et les uniforms
que les shaders 3D de WE attendent.

**Ce que fait l'amont (vérifié dans le code, lecture seule) [Amont] :** l'implémentation de référence **ne rend pas
les modèles 3D**.

- Un objet portant la clé `model` n'est reconnu comme aucun type. Il devient un objet générique sans rendu, avec un
  message « type inconnu ».
- Un objet `light` donne le message « lumières non prises en charge » et devient aussi un objet générique.
- `general.orthogonalprojection` est obligatoire. Absente ou `null`, toute la scène échoue (spec 02 Q6).
- `type` est obligatoire dans `project.json`. Les projets `audiophile` et `techno`, qui n'en ont pas, ne se chargent
  pas.
- Le module `#require LightingV1` est remplacé par une fonction qui renvoie toujours zéro (spec 03 §4.2).
- Aucune RT n'a de tampon de profondeur (spec 04 §11.2). `_rt_shadowAtlas` est créée mais jamais remplie.
- La seule perspective existante est celle des particules dont le bit 4 de `flags` est levé (œil fixe en (0, 0, 1000),
  spec 05 §2.3).

Conséquence : aucune des huit scènes perspective installées ne se charge dans l'amont. La scène 2D `retro` se charge,
mais son modèle de fond est ignoré.

`docs/PARITY-POLICY.md`, règle 4 : Lumenwall DOIT combler ce manque en suivant WE. C'est un sur-ensemble de l'amont.
Quand WE ne documente pas un détail et que les données ne permettent pas de trancher, la règle est marquée
**[Inféré]** et renvoie à une question ouverte (§5).

### Priorités

| Niveau | Contenu | Pourquoi |
|---|---|---|
| **P1** | `.mdl` statiques (§1.10), objets modèle, skins, sprite, caméra perspective et chemins (§3.2), profondeur (§3.1), interface d'éclairage héritée (§2.3), réflexion plane `_rt_Reflection` (§3.7), images plein écran dans une scène 3D (§3.9), objets sans `id` (§1.8) | Nécessaire aux 8 scènes 3D installées. |
| **P2** | Module `LightingV1` sans ombres (§2.4), shader `generic4` et dérivés, brouillard (§2.7), `_rt_MipMappedFrameBuffer` avec mips (§3.8), modèles dans une scène 2D (§3.12), tri des transparents (§3.6), fondu et tremblement de caméra (§3.2.5, §3.2.6) | Fonctions WE documentées, présentes dans les assets et les scènes d'aperçu, absentes des fonds installés (sauf fondu et tremblement : `ricepod`). |
| **P3** | Ombres (§2.5), volumétriques (§2.6), HDR (§2.8), squelette et animation des modèles 3D (§1.10.6), morphing | Documentés, rares, format incomplet. |

## Sources et étiquettes

| Étiquette | Signification |
|---|---|
| **[Doc WE]** | Documentation publique docs.wallpaperengine.io : *Getting started with 3D Models*, *3D Advanced Lighting*, *3D Camera Setup*, *3D Distance Fog*, *3D Model Shaders*, *Real-Time Lighting & Reflections*, *Tips for Advanced Lighting*, *Custom Geometry in SceneScript*, et les classes SceneScript `IScene`, `ILayer`, `IModelLayer`, `IModelData`, `CameraTransforms`. |
| **[Observé]** | Analyse des données installées, en lecture seule : 8 scènes 3D et `retro` dans `projects/defaultprojects`, 29 fichiers `.mdl`, shaders et matériaux de `assets/`, scènes d'aperçu `assets/scenes/*` et `assets/presets/*`, images `preview.jpg`. Aucun fichier n'est copié dans le dépôt. |
| **[Inféré]** | Déduction cohérente avec les données, non confirmée. Chaque cas renvoie à une QO. |
| **[Amont]** | Comportement de l'implémentation de référence, pour mémoire. |
| **[Shader WE]** | Fait de format lu dans un shader WE du dossier `assets/shaders` : nom d'uniform, annotation, combo, constante ou formule. |

Les tests sur données réelles sont `#[ignore]` et lisent `LUMENWALL_WE_ASSETS` (voir `docs/TESTING.md`). Le dossier
des projets officiels s'en déduit : `$LUMENWALL_WE_ASSETS/../projects/defaultprojects`.

---

## Corpus 3D installé

[Observé] Aucun élément du Workshop installé (14 éléments, dont 8 scènes ; `scene.json` lu dans les `scene.pkg`) ne
contient d'objet modèle. Toutes les scènes 3D viennent des projets officiels :

| Projet | Fichier de scène | Projection | Objets modèle (version `.mdl`) | Lumières | Shaders des matériaux | Particularités |
|---|---|---|---|---|---|---|
| `arsenal` | `scene.json` | perspective (`orthogonalprojection` absente) | `pistols` (`MDLV0014`, 6 maillages, format de sommet `0x27`) | 2 × `point` | `generic` (hérité) : combos `lightmap`, `normalmap`, `reflection`, `detailinalpha` | 2e jeu d'UV (lightmap), `_rt_Reflection` jamais alimentée, 5 chemins de caméra, bloom |
| `audiophile` | `audiophile.json` | perspective | `bars` (`reflected`), `flow`, `glow`, `grid` avec `skin: 1` (tous `MDLV0004`) | — | propres au projet : `audiophile`, `audiophileflow`, `audiophileglow`, `grid` | **pas de `type`** dans `project.json`, **objets sans `id` ni `name`**, audio |
| `demon_core` | `scene.json` | perspective | `core`, `backgroundsphere` (`MDLV0014` ; la sphère a des normales vers l'intérieur) | 1 × `point` | propres : `core` (lit `g_LightsPosition`, `g_LightsColorRadius`), `backgroundsphere` | particules 3D, 4 chemins dont la durée diffère du dernier instant clé, audio |
| `dna_fragment` | `scene.json` | perspective | `bg`, `bgfade` (quadrilatères en espace clip), `dna`, `curve` × 2 avec angles 3D (`MDLV0004`) | — | propres : `bg`, `dna`, `curve` | particules, 1 chemin immobile |
| `fantasticcar` | `fantasticcar.json` | perspective | `dome` et `body` (`reflected`), `grid`, `shadow` (`MDLV0014`) | — | propres : `car`, `dome`, `grid`, `shadow` | réflexion plane, `usershadervalues`, 6 chemins, bloom 2 / 0,65 |
| `neon_sunset` | `scene.json` | perspective (`orthogonalprojection: null`) | `neonsun`, `neongrid` (`MDLV0014`) | — | propres : `neonsun`, `neongrid`, `cloudsbg` | 2 images `fullscreen` (fond et calque d'effet `filmgrain`), particules |
| `ricepod` | `ricepod.json` | perspective (`null`) | `skybox`, `orbitaleffects` (3 maillages), `ricepod` (3), `jet` × 3 (`MDLV0014`) | — | propres : `skybox`, `ricepod`, `ricepodjet`, `ricepodorbital*`, `sprite` | **objet `sprite`**, particules `spritetrail`, `camerashake`, `camerafade`, clés `depthwriting` / `depthtesting` |
| `techno` | `techno.json` | perspective | `glow`, `orbitsmall` (4 maillages), `rays` (`MDLV0023`), `sphere` (`MDLV0014`) | — | propres : `technoglow`, `technohex`, `technoorbit` | pas de `type`, objets sans `id` |
| `retro` (2D) | `scene.json` | orthographique 2520 × 1080 | `bgfade` (`MDLV0014`, quadrilatère en espace clip) | — | propre : `bg` | modèle dans une scène 2D |

Données de référence hors fonds d'écran : `assets/scenes/modeleditor/scene.json` (gabarit d'une scène 3D : deux
lumières `lpoint`, `lightconfig`, `fov`, `nearz`, `farz`), `assets/scenes/particleelementpreviews/collisionmodel`
(modèle `MDLV0023` dans une scène 2D, matériau `generic4`, lumière `lpoint` avec `castshadow`, distances de cascade,
volumétriques) et `assets/models/editor/camera/camera.mdl` (`MDLV0017`).

Les fichiers `.obj`, `.mtl` et `.fbx` présents à côté des `.mdl` sont des sources d'éditeur. Lumenwall NE DOIT PAS
les lire. [Observé] Positions et UV de `grid.obj` se retrouvent dans `grid.mdl`, avec `v_mdl = 1 − v_obj`.

---

## 0. Conventions

### 0.1 Types

Types de la spec 01 §0.1 et de la spec 08 §0.1 : `u8`, `u16`, `u32`, `f32` (little-endian), `cstr`, `magic9`, `mat4`
(colonne-majeure, convention vecteur-colonne).

### 0.2 Repère monde

- **[Doc WE]** Axe avant −Z, axe haut +Y (export FBX par défaut de Blender). Le repère est **main droite**.
- **[Doc WE]** Unités : dans une scène 3D, une unité standard vaut 1,0 ; dans une scène 2D, une unité vaut un pixel,
  y compris pour une forme 3D.
- **[Observé]** Les positions d'un `.mdl` sont exprimées dans ce repère, sans mise à l'échelle par rapport à la source
  `.obj`.
- **[Observé]** Les faces avant sont parcourues dans le **sens trigonométrique** autour de leur normale. Sur 18
  maillages pourvus de normales (8 projets), le produit vectoriel `(b − a) × (c − a)` a le même sens que la somme des
  normales des sommets pour 99,9 % des triangles (24 exceptions sur environ 27 600, sur des pièces fines). [Doc WE] Le
  tutoriel SceneScript parle lui aussi d'ordre « CCW ».

### 0.3 Convention d'espace clip de WE (« convention WE »)

WE tourne sur Direct3D 11. Ses shaders 3D supposent la convention suivante :

| Grandeur | Convention WE | Preuves |
|---|---|---|
| `y` clip | `y_ndc = +1` désigne le **haut** de la cible | [Observé] `dna_fragment` : le shader `bg` place `a_TexCoord.y = 1` en `y_ndc = +1` et y affiche `tint2` (propriété `bgcolor`, vert). `preview.jpg` est vert en haut. [Shader WE] `volumetricsfront.frag` et `common_pbr_2.h` (`CalculateProjectedCoords`) passent de NDC à UV par `(x·0,5 + 0,5, 0,5 − y·0,5)` sans condition. |
| profondeur | `z_ndc ∈ [0, 1]`, 0 au plan proche | [Shader WE] `volumetricsfront.vert` (combo `FULLSCREEN`) écrit `z = 0` hors `REVERSEDEPTH` et `z = 1` sinon. `occlusiontest.vert` écrit `z = 0,999·w` hors `REVERSEDEPTH`. |
| texture | `v = 0` désigne la ligne 0, en haut | spec 04 §0.2, point 3 |

C'est **l'inverse** de la convention « espace clip amont » de la spec 04 §0.2 point 4 (ligne 0 ↔ `y_ndc = −1`,
profondeur [−1, 1]), que l'amont tient d'OpenGL. L'invariant de stockage (ligne 0 = haut) est commun aux deux.

Règle d'implémentation recommandée (**DEVRAIT**) : les **passes 3D** utilisent la convention WE, sans la correction
`(x, −y, (z + w)/2, w)` de la spec 04 §24.2. Sont des passes 3D : celles des objets modèle et sprite, et les passes
d'ombre, d'occlusion et de volumétrie. Pour les passes 3D :

1. Les matrices fournies par le moteur sont construites en convention WE (§3.2.2).
2. La position clip écrite par le shader va telle quelle à wgpu. wgpu a la même convention que D3D : y vers le haut,
   z dans [0, 1].
3. Le shader est compilé avec la macro **`HLSL` définie à 1**, en plus de `GLSL` = 1 (spec 03 §3.3). `HLSL_SM30`
   reste **non définie** : WE cible Direct3D 11. Ces passes activent donc les adaptations D3D écrites par WE dans
   ses shaders :
   - `v_ScreenPos.y` négatif (`generic4.vert`, `base/model_vertex_v1.h`) ;
   - `normal.y` négatif dans la réflexion (`generic3/4.frag`, `base/model_fragment_v1.h`) ;
   - décalage d'un demi-pixel (`g_TexelSize` en unités NDC) des quadrilatères en espace clip (`bg.vert` de
     `dna_fragment` et `retro`) ;
   - variante HLSL de l'atténuation dans `ComputePBRLightShadow`.
4. `ddy` y désigne la dérivée native, dans le sens des lignes croissantes, **sans** la négation de la spec 03 §3.3
   (QO-3D-22).

Exigence observable (**DOIT**, quelle que soit l'implémentation) : sur le fixture F-3D-O (§4.6), l'image produite
DOIT avoir la même orientation que celle de WE.

Les passes de la chaîne d'images de la spec 04 restent dans leur propre convention, même dans une scène 3D. Les deux
conventions respectent l'invariant « ligne 0 = haut » : leurs sorties se composent donc sans retournement.

### 0.4 Matrices dans les shaders

Comme dans la spec 03 §3.3 : `mul(v, M)` devient `M · v`, et les matrices sont téléversées en colonne-majeure, non
transposées. Les matrices ci-dessous sont donc écrites en convention vecteur-colonne.

---

## 1. Données

### 1.1 Reconnaître une scène 3D

- Une scène est **perspective** quand `general.orthogonalprojection` est **absente** ou vaut **`null`**. [Observé] Les
  8 scènes 3D sont dans ce cas, ainsi que le gabarit `modeleditor`. Les 177 autres `scene.json` installés qui
  portent une projection sont orthographiques. Lumenwall NE DOIT PAS échouer dans ce cas, contrairement à l'amont
  (amende la spec 02 Q6).
- Un `project.json` sans `type` dont `file` se termine par `.json` DOIT être traité comme une scène [Inféré, Observé
  sur `audiophile` et `techno`] (QO-3D-1).
- Taille de rendu d'une scène perspective : il n'y a ni largeur ni hauteur de projection. La cible de scène
  (`_rt_FullFrameBuffer`) a la taille de la sortie, ou celle du groupe en mode span (spec 07 §4.4) [Inféré].
  `W × H` désigne cette taille dans la suite.

### 1.2 `scene.json` : caméra et réglages généraux utiles à la 3D

`camera` (objet) :

| Clé | Type | Défaut | Rôle |
|---|---|---|---|
| `eye` | vec3 | — | Position de la caméra. [Doc WE] `CameraTransforms.eye` |
| `center` | vec3 | — | Point visé. |
| `up` | vec3 | — | Haut. Il n'est pas forcément orthogonal à la visée (`arsenal` : `(−0,165, 0,986, 0,028)`). La matrice de vue l'orthonormalise (§3.2.1). |
| `paths` | tableau de chaînes | absent | Chemins de fichiers de trajectoires (§1.3), ex. `scripts/camera_00.json`. [Observé] présent dans 6 scènes. |

`general`, clés propres à la 3D. [Observé] Dans les gabarits, `fov`, `nearz` et `farz` sont dans **`general`**, pas
dans `camera` (spec 02 Q5, spec 04 Q4). Lumenwall DOIT les lire dans `general`, puis à défaut dans `camera`.

| Clé | Type | Défaut | Rôle |
|---|---|---|---|
| `fov` | nombre | **50** | [Doc WE] « Field of view in 3D », en degrés. Champ **vertical** [Inféré] (QO-3D-2). [Observé] 50 dans 97 gabarits sur 98. Sur `neon_sunset`, la taille du soleil dans `preview.jpg` (≈ 626 px sur 1152) concorde avec 50° (671 px attendus pour le quad entier, dont le disque occupe ≈ 93 %). |
| `nearz` | nombre | **0,01** [Inféré : gabarits 0,0099999998] | [Doc WE] plan proche. |
| `farz` | nombre | **10000** [Inféré : gabarits] | [Doc WE] plan lointain. |
| `zoom` | nombre | 1 | [Doc WE] « only used on 2D wallpapers ». Ignoré en perspective. |
| `perspectiveoverridefov` | nombre | absent | [Observé] 95 ou 90,76, seulement dans des scènes **2D**. Champ de vision des éléments en perspective d'une scène 2D (§3.12, QO-3D-3). |
| `ambientcolor` | couleur | (0, 0, 0) | [Doc WE] « The ambient color in 3D » (couleur du haut). |
| `skylightcolor` | couleur | (0, 0, 0) | [Doc WE] « The lower ambient color in 3D ». |
| `clearcolor` | couleur | (1, 1, 1) [spec 02] | Couleur d'effacement. |
| `clearenabled` | booléen | vrai | [Doc WE] efface la cible au début de la frame. `null` [Observé, 45 fois] vaut vrai. |
| `camerafade` | booléen | faux | [Doc WE] fondu au changement de chemin (§3.2.5). |
| `camerashake`, `camerashakeamplitude`, `camerashakeroughness`, `camerashakespeed` | booléen, nombres | faux, 0, 0, 0 | [Doc WE] tremblement (§3.2.6). |
| `cameraparallax*` | — | spec 02 §6.3 | En perspective : §3.2.7. |
| `bloom`, `bloomstrength`, `bloomthreshold` | — | voir §3.10 | Bloom. |
| `hdr`, `bloomhdr*` | booléen, nombres | faux | HDR (§2.8, P3). |
| `lightconfig` | objet | absent | [Observé] `{"point": 2}` ou `{"point": 1, "pointshadow": 1}`. Cache éditeur du nombre de lumières par type (§2.2). |
| `transparentsorting` | booléen | faux [Inféré] | Tri des transparents (§3.6). |
| `norecompile`, `camerapreview`, `gravity*`, `wind*`, `version` | — | — | Sans effet sur le rendu 3D. |

Clés de brouillard : le nom des clés JSON est inconnu (aucune occurrence dans le corpus) (QO-3D-4).

### 1.3 Fichier de chemins de caméra

[Observé] Fichier JSON référencé par `camera.paths`. Une seule clé racine, `paths`, tableau de chemins :

| Clé (chemin) | Type | Rôle |
|---|---|---|
| `duration` | nombre (s) | [Doc WE] « Duration: set in seconds ». |
| `name` | chaîne | Toujours vide. |
| `transforms` | tableau | Images clés. |

| Clé (image clé) | Type | Rôle |
|---|---|---|
| `timestamp` | nombre | Instant de l'image clé. Le premier vaut 0. |
| `eye`, `center`, `up` | vec3 (chaîne) | Pose de la caméra. |
| `zoom` | nombre | 1 partout. [Doc WE] Sans effet en 3D. |

[Observé] 1 à 6 chemins par scène, 1 ou 2 images clés par chemin. Dans 5 scènes, le dernier `timestamp` vaut
`duration`. **Dans `demon_core`, ce n'est pas le cas** : durées 300, 450, 400, 350 pour des derniers instants 40, 34,
40, 36. Voir §3.2.4 et QO-3D-5.

[Doc WE] Les modes de lecture *Single*, *Loop*, *Mirror* et l'ordre aléatoire ou séquentiel existent dans l'éditeur.
Leurs clés JSON n'apparaissent dans aucun fichier du corpus (QO-3D-6).

### 1.4 Classification des objets (amende la spec 02 §7.1)

Une clé à valeur `null` ou chaîne vide compte comme **absente**. [Observé] Les lumières héritées portent `"model":
null`, `"particle": null` et `"sprite": null`. `neon_sunset` contient un objet `"image": null, "model": null,
"particle": "particles/stars.json"`.

Ordre de test, le premier critère satisfait l'emporte :

1. `image` chaîne → image (spec 02 §7.3) ;
2. `sound` tableau → son ;
3. `particle` (règle de la spec 02) → particules ;
4. `text` présent et non `null` → texte ;
5. **`model` chaîne non vide → modèle** (§1.5) ;
6. **`light` chaîne non vide → lumière** (§1.6) ;
7. **`sprite` chaîne non vide → sprite** (§1.7) ;
8. `shape` → lumière volumique (non couverte, objet générique) ;
9. sinon objet générique.

Le rang du critère « modèle » parmi 1 à 4 est sans effet observable : aucun objet du corpus ne porte deux types.

### 1.5 Objet modèle

[Observé] Clés rencontrées (8 scènes, 2 scènes d'aperçu) :

| Clé | Mode (spec 02 §0.4) | Défaut | Rôle |
|---|---|---|---|
| `model` | R chaîne | — | Chemin du `.mdl` (résolution spec 01 §4.7). |
| `id`, `name`, `parent`, `dependencies` | spec 02 §7.2 | — | Communs. **Peuvent manquer** (§1.8). |
| `origin` | L vec3 | (0, 0, 0) | Position (repère monde, §0.2). |
| `angles` | L vec3 | (0, 0, 0) | Rotation en **radians** [Observé : 3,142]. [Doc WE] Les scripts la voient en **degrés** (`ILayer.angles`). |
| `scale` | L vec3 | (1, 1, 1) | Échelle. |
| `visible` | L booléen | vrai | Visibilité. |
| `skin` | O entier | 0 | Index de skin (§1.11.1). [Observé] `audiophile` : `"skin": 1`. |
| `reflected` | O booléen | faux | Dessiné aussi dans `_rt_Reflection` (§3.7). [Observé] `fantasticcar` (dôme, voiture), `audiophile` (barres). |
| `castshadow` | O booléen | vrai | [Doc WE] « models have shadows enabled by default ». Nom de clé [Inféré] d'après la clé `castshadow` des lumières. P3. |
| `perspective` | O booléen | faux | Scène 2D seulement (§3.12). [Doc WE] `IModelLayer.perspective`. |
| `parallaxDepth` | L vec2 | (0, 0) | §3.2.7. |
| `locktransforms` | — | — | Éditeur, ignoré. |
| `alpha`, `color`, `brightness` | L | 1, blanc, 1 | Absents du corpus pour les modèles. S'ils sont présents : uniforms `g_Alpha`, `g_Color4`, `g_Brightness` (§2.9) [Inféré]. |

Un objet modèle n'a **pas** d'effets 2D (aucune clé `effects` observée).

### 1.6 Objet lumière

| Clé | Type | Défaut | Rôle |
|---|---|---|---|
| `light` | chaîne | — | Type. [Observé] `point` (scènes 2016) et `lpoint` (gabarits récents). [Inféré] aussi `spot`/`lspot`, `tube`/`ltube`, `directional`/`ldirectional` (QO-3D-7). Valeur inconnue : avertissement, lumière ignorée. |
| `origin` | L vec3 | (0, 0, 0) | Position monde, après la hiérarchie `parent` (§3.3). |
| `angles` | L vec3 | (0, 0, 0) | Orientation des lumières spot et directionnelles. Direction d'émission = `R(angles)·(0, 0, −1)` [Inféré, axe avant −Z §0.2]. |
| `color` | Lc couleur | (1, 1, 1) | Couleur, en flottants [Observé « 0.890 0.690 0.086 »]. |
| `intensity` | L nombre | 1 | [Doc WE] luminosité. [Observé] 0,18 à 6,44. |
| `radius` | L nombre | — | [Doc WE] portée. [Observé] 16,32 et 22,85 (`arsenal`), 811,69 en pixels (aperçu 2D). |
| `exponent` | L nombre | 2 [Inféré, QO-3D-8] | Exposant d'atténuation (§2.4). [Observé] 2,0. |
| `castshadow` | O booléen | faux [Inféré] | [Doc WE] « Lights must have the Cast shadow option enabled ». |
| `cascadedistance0..2` | nombres | — | [Observé] 0, 100, 200. Cascades des lumières directionnelles (§2.5). |
| `density`, `volumetricsexponent` | nombres | — | [Observé] 7,48 et 4,0. Volumétrie (§2.6). |
| `innercone`, `outercone` (noms supposés), `cookie` | — | — | [Doc WE] cônes intérieur et extérieur, projection de texture des spots. Noms de clés inconnus (QO-3D-7). |
| `visible`, `parent`, `scale`, `parallaxDepth`, `name`, `id` | — | — | Communs. Une lumière invisible est exclue (§2.2) [Inféré]. |

### 1.7 Objet sprite

[Observé] `ricepod`, objet « sun » : `"sprite": "materials/sprites/sunsprite.json"`. La valeur est le chemin d'un
**matériau** (spec 02 §9), pas d'un modèle. Avec les clés communes (`origin`, `angles`, `scale`, `visible`,
`parallaxDepth`).

Rendu (§3.5.3) : billboard tourné vers la caméra et dessiné par le shader du matériau. Le moteur fournit 4 sommets de
même `a_Position = (0, 0, 0)`, avec `a_TexCoord` aux 4 coins (0, 0), (1, 0), (1, 1), (0, 1), en 2 triangles [Inféré
d'après `sprite.vert` de `ricepod` et `occlusiontest.vert`]. Ces shaders écartent les coins de
`(u − 0,5)·g_ViewRight + (v − 0,5)·g_ViewUp`, multiplié par 0,5.

[Shader WE] Conséquence : la ligne 0 de la texture (v = 0) est dessinée vers le **bas** du billboard. Lumenwall NE
DOIT PAS corriger ce retournement, qui fait partie du rendu WE.

### 1.8 Objets et projets « anciens » sans identifiant

[Observé] `audiophile.json` et `techno.json` : les objets n'ont ni `id`, ni `name`, ni `angles`, ni `scale`. Les
défauts s'appliquent (`origin` (0, 0, 0)…). Lumenwall DOIT créer et dessiner **tous** ces objets. Il leur attribue des
identifiants synthétiques distincts, qui ne rencontrent aucun `id` du fichier : par exemple `−1000 − rang`
[Inféré]. Le mode dégradé de la spec 02 §7.2 (`id` = −1 pour tous) les réduirait à un seul objet (spec 02 Q17).

### 1.9 Fichier compagnon `models/<nom>.json`

[Observé] Facultatif : absent pour `retro/bgfade`, l'aperçu `collisionmodel`, `audiophile/flow` et `glow`, et une
partie de `techno`. Quand il existe, il porte le même nom que le `.mdl`, extension `.json`, dans le même dossier
[Inféré].

| Clé | Type | Rôle |
|---|---|---|
| `normals` | booléen | Option d'import : normales présentes. [Observé] toujours cohérente avec le bit `0x2` du format de sommet. |
| `tangentspace` | booléen | Option d'import : tangentes. Cohérente avec le bit `0x4`. |
| `seconduvchannel` | booléen | Option d'import : 2e jeu d'UV. Cohérente avec le bit `0x20` (`pistols`). |
| `skins` | tableau d'objets `{nom_matériau: nom_remplaçant}` | [Observé] `audiophile/grid.json` : `[{"grid": "grid"}, {"grid": "grid2"}]`. Table des skins (§1.11.1). |

Ces options sont **informatives**. Le format réel est celui du `.mdl` (§1.10.3), qui fait foi.

### 1.10 Format binaire `.mdl` statique (complète la spec 08 §1–§3 et la spec 01 §3.1)

La spec 08 décrit `MDLV0023` avec un squelette. Cette section couvre les quatre versions statiques observées. Les
29 fichiers `.mdl` du dossier d'installation sont lus exactement par cet algorithme : fin de lecture à `taille − 1`
(octet terminal `0x00`) ou à `taille` pour `MDLV0004` [Observé, table §4.7].

#### 1.10.1 En-tête

| Décalage | Type | Champ | Observé |
|---|---|---|---|
| 0 | `magic9` | `MDLV` + 4 chiffres + `0x00` | `MDLV0004`, `MDLV0014`, `MDLV0017`, `MDLV0023` |
| 9 | `u32` | drapeaux du modèle | Pour `0004` et `0014`, c'est **le format de sommet** de tous les maillages (§1.10.3) : `0x9`, `0xB`, `0xF`, `0x27`. Pour `0017` et `0023`, il est informatif : égal au format des maillages sur les modèles statiques, `0x01800009` sur les marionnettes. |
| 13 | `u32` | `S` = **nombre de skins** | 1 partout, sauf `audiophile/models/grid/grid.mdl` (`MDLV0004`) : **2**. C'est le champ « réservé » de la spec 08 §2.1 (1, ou 2 sur un `MDLV0004`). `S = 0` est invalide. |
| 17 | `u32` | `nm` = nombre de maillages | 1 à 6. |
| 21 | — | `nm` maillages (§1.10.2) | |

`N` désigne la version, c'est-à-dire l'entier formé par les 4 chiffres.

#### 1.10.2 Maillage (sous-maillage, un matériau par skin)

| Ordre | Type | Champ | Condition |
|---|---|---|---|
| 1 | `S` × `cstr` | chemins de matériau, un par skin, ex. `materials/pistols/knife_df.json` | toujours |
| 2 | `u32` | réservé, 0 partout | toujours |
| 3 | 6 × `f32` | boîte englobante (min xyz, max xyz) en espace modèle | `N ≥ 17` |
| 4 | `u32` | `vf` = format de sommet du maillage | `N ≥ 17` (sinon `vf` = drapeaux de l'en-tête) |
| 5 | `u32` | `vb` = taille des sommets en octets | toujours |
| 6 | `vb` octets | sommets (§1.10.3) | |
| 7 | `u32` | `ib` = taille des indices en octets | toujours |
| 8 | `ib` octets | indices `u16`, liste de triangles | |
| 9 | — | queue de maillage de la spec 08 §2.6 | `N ≥ 23` seulement. [Observé] 6 octets nuls sur les statiques. |

[Observé] Les versions `0004` et `0014` n'ont **ni boîte, ni `vf` par maillage**. La version `0017` a boîte et `vf`
mais pas de queue (`camera.mdl`).

#### 1.10.3 Format de sommet (complète la spec 08 §2.3)

Les champs se suivent dans l'ordre de la table, sans remplissage :

| Bit | Masque | Champ | Type | Attribut shader |
|---|---|---|---|---|
| 0 | `0x00000001` | position | 3 × `f32` | `a_Position` |
| 1 | `0x00000002` | normale | 3 × `f32` | `a_Normal` |
| 2 | `0x00000004` | tangente, `w` = signe de la bitangente | 4 × `f32` | `a_Tangent4` |
| 23 | `0x00800000` | indices d'os | 4 × `u32` | `a_BlendIndices` |
| 24 | `0x01000000` | poids d'os | 4 × `f32` | `a_BlendWeights` |
| 3 | `0x00000008` | UV | 2 × `f32` | `a_TexCoord` |
| **5** | **`0x00000020`** | **deux jeux d'UV** : (u0, v0, u1, v1) | **4 × `f32`** | `a_TexCoordVec4`. `a_TexCoord` reçoit (u0, v0). |

[Observé] Le bit `0x20` n'apparaît que sur `arsenal/pistols.mdl` (`vf = 0x27`, 56 octets par sommet). Les 6 tailles
`vb` sont des multiples exacts de 56, alors qu'elles ne le sont pas de 40. Normales et tangentes y sont unitaires,
orthogonales (|n·t| < 3e-7), avec `w ∈ {−1, +1}`. (u1, v1) reste dans [0, 1], comme une lightmap. Les bits `0x8` et
`0x20` ne sont jamais réunis : un format qui les porte tous deux DOIT être refusé (QO-3D-9). Tout autre bit est
inconnu et DOIT faire refuser le maillage.

[Observé] Tailles rencontrées : `0x9` → 20 o, `0xB` → 32 o, `0xF` → 48 o, `0x27` → 56 o.

Contenu des champs [Observé] :

- normales unitaires, dans le sens des faces avant (§0.2). Exception : `demon_core/backgroundsphere.mdl`, dont les
  1087 normales pointent vers l'**intérieur** (sphère vue de l'intérieur ; ses triangles tournent dans le sens
  trigonométrique autour de ces normales).
- UV avec origine en **haut à gauche** : `v = 1 − v_obj`. Sur `neonsun.mdl`, le sommet `y = +0,5` a `v = 0`. Les UV
  peuvent sortir de [0, 1] (−7,7 à 8,7 sur `grid`) : la répétition vient de l'échantillonneur.
- tangentes : `bitangente = (n × t)·w` [Shader WE] (`BuildTangentSpace`, `common_vertex.h`). [Observé] `w = +1` sur
  les imports anciens. [Doc WE] Le tutoriel `IModelData` recommande `w = −1`, « because texture space has an inverted
  Y-axis ». Les deux signes sont valides : le moteur ne les modifie pas.

#### 1.10.4 Fin de fichier et sections

Après le dernier maillage, le chaînage des sections de la spec 08 §3 s'applique tel quel : fin si `p = taille` ou si
`D[p] = 0x00`. [Observé] `MDLV0004` se termine **sans** octet terminal ; `0014`, `0017` et `0023` statiques ont un seul
octet `0x00` final.

#### 1.10.5 Lecture et validation

1. Lire l'en-tête, rejeter si `magic` ≠ `MDLV` + 4 chiffres + `0x00` ou si `S = 0`.
2. Pour chaque maillage : lire `S` chaînes, le `u32` réservé, puis boîte et `vf` si `N ≥ 17`. Taille de sommet
   `T(vf)` selon §1.10.3 ; bit inconnu → refus.
3. `vb` DOIT être un multiple de `T(vf)`, `ib` un multiple de 6. Tout indice DOIT être `< vb / T(vf)`. Toute lecture
   DOIT rester dans le fichier.
4. `N ≥ 23` : lire la queue (spec 08 §2.6).
5. Sections chaînées (spec 08 §3).

Versions : `0004`, `0014`, `0017` et `0023` DOIVENT être acceptées. Une autre version (`0021`, puis `0005`–`0013`,
`0015`, `0016`, `0018`–`0022`) PEUT être tentée avec la règle « boîte et `vf` si `N ≥ 17`, queue si `N ≥ 23` »
[Inféré] (QO-3D-10). Tout échec est **non fatal** : l'objet modèle n'est pas dessiné, avec un avertissement qui donne
le chemin, la version, le décalage et la cause (règle 2 de la politique de parité).

Aiguillage avec la spec 08 : un `.mdl` référencé par un **objet modèle** est lu ici. Ses sections `MDLS`/`MDLA`
éventuelles relèvent du §1.10.6. Un `.mdl` référencé par la clé `puppet` d'un modèle d'**image** reste traité par la
spec 08.

#### 1.10.6 Modèles 3D avec squelette (P3)

[Doc WE] WE importe des modèles FBX animés (« Model Animation Setup », `IAnimationLayer` : « Puppet warp or 3D model
animation layers »). Aucun modèle 3D animé n'est installé. Si un `.mdl` d'objet modèle porte `MDLS` (et `MDLA`), on
applique la spec 08 §9 avec ces différences [Inféré] :

- l'espace maillage est l'espace modèle 3D (pas de conjugaison `C` de la spec 08 §9.8) ;
- `g_Bones[b]` reçoit les 3 premières lignes de `K_b` directement ;
- combos `SKINNING` = 1 et `BONECOUNT` = `max(1, n)`.

Les rotations d'os en 3D dépassent la spec 08 (angles d'Euler sur trois axes) (QO-3D-11).

### 1.11 Matériaux des modèles

#### 1.11.1 Choix du matériau d'un maillage

Matériau du maillage `i` pour la skin `k` (clé `skin` de l'objet) :

- si `k < S` : la `k`-ième chaîne du maillage ;
- sinon : la chaîne 0, avec un avertissement [Inféré].

[Observé] Sur `audiophile/grid`, cette règle donne le même résultat que la table `skins` du compagnon : skin 1 →
`grid` → `grid2`, soit `materials/grid/grid2.json`. La table `skins` est redondante ; Lumenwall PEUT l'ignorer (QO-3D-12).

Le chemin est résolu tel quel (spec 01 §4.7). Contrairement aux images, aucun `models/*.json` n'intervient : le
matériau vient du `.mdl`.

#### 1.11.2 États par défaut d'une passe de matériau de modèle

**[Inféré, P1] Valeurs par défaut pour une passe de matériau appartenant à un objet modèle ou sprite :**
`depthtest` = **enabled**, `depthwrite` = **enabled**, `cullmode` = **normal**, `blending` = `normal`. Elles
remplacent celles de la spec 02 §9 et de la spec 04 §11.2 (tout `disabled` / `nocull`), qui restent celles des
images. Indices [Observé] :

- les matériaux `generic` d'`arsenal`, `dna.json`, `core.json` et `separatistship*.json` ne précisent aucun état.
  C'est la géométrie opaque du modèle, qui ne peut s'afficher correctement sans test de profondeur ;
- `fantasticcar/materials/car/glass.json` contient `"cullmode": "nocull"` **commenté**. Le défaut est donc autre chose
  que `nocull` ;
- `backgroundsphere` (normales intérieures, aucun état) ne s'affiche de l'intérieur qu'avec l'élimination des faces
  arrière et l'orientation trigonométrique (§0.2) ;
- `occlusiontest.json` ne désactive que `depthwrite`, et `shadowcaster.json` ne précise rien alors qu'il doit écrire
  la profondeur ;
- `skybox.json`, `bg.json`, `bgfade.json` et les matériaux additifs de `techno` désactivent explicitement la
  profondeur.

Valeurs reconnues : celles de la spec 02 §9. [Observé] Valeurs du corpus : `cullmode` ∈ {`normal`, `nocull`}, `blending` ∈ {`normal`,
`translucent`, `additive`}, `depthtest`/`depthwrite` ∈ {`enabled`, `disabled`}.

Clés mal orthographiées `depthwriting` et `depthtesting` [Observé, `ricepod` : `orbital_*.json`, toutes à
`disabled`] : Lumenwall DEVRAIT les lire comme `depthwrite` et `depthtest` [Inféré, QO-3D-13]. Ces anneaux, centrés
sur l'œil (`position + g_EyePosition`) et dessinés avant le vaisseau, perceraient sinon le vaisseau avec leur
profondeur.

`alphawriting` (`default` / `enabled`) [Observé] : masque d'écriture de l'alpha, hors périmètre (spec 04 §11.6).

#### 1.11.3 `usershadervalues` (complète la spec 02 §9 et la spec 03 §7)

[Observé] Clé de passe de matériau, présente dans 6 projets 3D :
`{"<propriété utilisateur>": "<alias matériau>"}`. Par exemple `"carbodycolor": "paintcolor"` et
`"schemecolor": "ambientcolor"`.

Règle [Inféré] : l'uniform dont l'annotation porte `"material": "<alias>"` est lié **en direct** à la valeur de la
propriété utilisateur, comme une constante liée (spec 03 §7.5). Cette liaison l'emporte sur une entrée
`constantshadervalues` de même alias. Une propriété inexistante est ignorée, avec un avertissement. Conversion de
type : spec 03 §7.4. Une propriété `color` donne un vec3.

[Observé] La clé de propriété `shadername` de `techno/project.json` (`"tint"`) fait doublon avec `usershadervalues` :
elle est ignorée [Inféré].

#### 1.11.4 Combos des matériaux de modèles

[Observé] Les noms de combos sont souvent en minuscules (`lightmap`, `normalmap`, `reflection`, `detailinalpha`,
`metal`, `paintwork`, `selfillum`, `clouds`, `rays`). La mise en majuscules de la spec 03 §5.5 suffit.
`fantasticcar` fixe `"TEX1FORMAT": 4` explicitement.

**Annotation `formatcombo`** [Shader WE : `"formatcombo": true` sur `g_Texture1` (carte de normales) et `g_Texture4`
(dégradé) de `generic3/4`, `foliage4`, `fur4`, `chroma4`] : le moteur définit le combo `TEX<n>FORMAT`, où `n` est le
numéro du sampler. Sa valeur est le code de format WE de la texture liée à ce slot, s'il n'est pas déjà fixé par le
matériau ou la surcharge [Inféré, QO-3D-14]. Codes [Shader WE, `common_fragment.h`] : RGBA8888 = 0, RGB888 = 1,
RGB565 = 2, ETC1 = 3, DXT5 = 4, ETC2 = 5, DXT3 = 6, DXT1 = 7, RG88 = 8, R8 = 9, RG1616F = 10, R16F = 11, BC7 = 12.

Ce sont les valeurs du champ format des `.tex` (spec 01 §2.3). Une texture à image embarquée (PNG…) vaut 0. Exemple :
les cartes de normales `dxt5n` du corpus sont au format 4. `DecompressNormal` lit alors `x = 2a − 1` et
`y = 2g − 0,965`. Sans `TEX1FORMAT`, il lit `x = 2a − 1` et `y = 2g − 1`.

### 1.12 Shaders 3D de WE (catalogue des faits de format)

Shaders de `assets/shaders` liés à la 3D. Combos déclarés : ceux des annotations `[COMBO]`. Combos moteur :
symboles testés par `#if` sans déclaration, que le moteur doit fournir (§2.10).

| Shader | Rôle | Uniforms et attributs moteur | Combos déclarés | Combos moteur |
|---|---|---|---|---|
| `generic` (hérité, `arsenal`) | Phong / demi-Lambert, 4 lumières ponctuelles fixes | `g_ModelMatrix`, `g_ViewProjectionMatrix`, `g_EyePosition`, `g_LightsPosition[4]` (vec3), `g_LightsColorRadius[4]` (vec4), `g_LightAmbientColor`, `g_LightSkylightColor`, `g_TexelSizeHalf` ; `a_Position`, `a_Normal`, `a_Tangent4`, `a_TexCoord` ou `a_TexCoordVec4` | aucun : `LIGHTMAP`, `NORMALMAP`, `REFLECTION`, `DETAILINALPHA`, `DIFFUSETINT` viennent du matériau | — |
| `generic2` | idem, plus combo `NORMALMAP` déclaré | idem | `NORMALMAP` | — |
| `generic3` | PBR « obsolète » avec tableaux de lumières par type | + `g_NormalModelMatrix`, `g_Screen`, `g_Texture3MipMapInfo`, `g_LPoint_*`, `g_LSpot_*`, `g_LTube_*`, `g_LDirectional_*` | `LIGHTING`, `REFLECTION`, `RIMLIGHTING`, `SHADINGGRADIENT`, `TINTMASKALPHA` | `LIGHTS_POINT`, `LIGHTS_SPOT`, `LIGHTS_TUBE`, `LIGHTS_DIRECTIONAL` (nombres), `SHADERVERSION`, `HDR`, `SKINNING`, `BONECOUNT`, `MORPHING*` |
| **`generic4`** (shader par défaut, `declarations.json`) | PBR avec `#require LightingV1` | `g_ModelMatrix`, `g_NormalModelMatrix`, `g_ViewProjectionMatrix`, `g_EyePosition`, `g_LightAmbientColor`, `g_LightSkylightColor`, `g_Screen`, `g_Texture3MipMapInfo`, `g_Texture6Texel`, `g_Bones`, `g_Fog*` | `LIGHTING` (1), `FOG` (1), `REFLECTION` (0), `RIMLIGHTING`, `SHADINGGRADIENT`, `TINTMASKALPHA`, plus les combos de textures `NORMALMAP`, `PBRMASKS`, `METALLIC_MAP`, `ROUGHNESS_MAP`, `REFLECTION_MAP`, `EMISSIVE_MAP` | `LIGHTS_SHADOW_MAPPING`, `LIGHTS_SHADOW_MAPPING_QUALITY`, `LIGHTS_COOKIE`, `FOG_DIST`, `FOG_HEIGHT`, `HDR`, `ADDITIVE`, `ALPHATOCOVERAGE`, `REVERSEDEPTH`, `DOUBLESIDEDLIGHTING`, `SKINNING`, `BONECOUNT`, `MORPHING*`, `PLATFORM_ANDROID` |
| `foliage4`, `fur4`, `chroma4` | [Doc WE] « Vegetation », « Fur », « Chroma ». Construits sur `base/model_vertex_v1.h` et `base/model_fragment_v1.h`. | comme `generic4` | comme `generic4`, plus les leurs (`LEAVESUVMODE`, `FOLIAGEDEBUG`, `INSTANCECOUNT`…) | comme `generic4` |
| `shadowcaster`, `shadowcasterfoliage4`, `shadowcasterfur4` | Passe d'ombre (§2.5) | `g_ModelMatrix`, `g_ViewportViewProjectionMatrices[6]`, `gl_InstanceID`, `gl_ViewportIndex` | `SKINNING`, `MORPHING`, `MORPHING_NORMALS`, `BONECOUNT` | `ALPHATOCOVERAGE` |
| `occlusiontest` | Billboard d'occlusion au point de la lumière | `g_ModelViewProjectionMatrix`, `g_ViewUp`, `g_ViewRight` | — | `REVERSEDEPTH` |
| `volumetricsfront`, `volumetricsback` | Volumétrie (§2.6) | `g_ViewProjectionMatrix`, `g_AltViewProjectionMatrix`, `g_AltModelMatrix`, `g_EffectModelMatrix`, `g_RenderVar0..4`, `g_Texture1Resolution`, `g_Texture3Resolution` | `FOG` | `POINTLIGHT`, `SHADOW`, `COOKIE`, `QUALITY`, `FULLSCREEN`, `REVERSEDEPTH` |
| `genericimage4` (images) | Éclairage d'une image 2D, même module | — | `LIGHTING` (0), `REFLECTION`, `FOG` (1) | `SCENE_ORTHO`, `FOG_COMPUTED`, … |

Annotations propres à la 3D [Shader WE] :

- **`// [PASS] shadow <shader>`** en tête d'un fragment (`foliage4` → `shadowcasterfoliage4`, `fur4` →
  `shadowcasterfur4`, `chroma4` → `shadowcaster`) : nom du shader de la passe d'ombre de ce matériau. Sans
  annotation, le moteur prend `shadowcaster` [Inféré]. L'analyse ligne à ligne de la spec 03 §4.3 DOIT reconnaître le
  marqueur `// [PASS] ` (avec espace) et ne pas le prendre pour une annotation d'uniform.
- **`// [COMBO_DISABLED] {…}`** (`generic3`) : déclaration désactivée. Elle NE DOIT PAS être traitée comme `[COMBO]`.
  La sous-chaîne `// [COMBO] ` ne s'y trouve pas, donc la spec 03 l'ignore déjà.
- Clés d'annotation de sampler `hidden`, `nonremovable`, `components`, `paintdefaultcolor`, `mode`, `formatcombo`
  (§1.11.4). Seules `default`, `combo`, `require`, `requireany` et `formatcombo` ont un effet moteur.

Shaders propres aux projets [Observé] : ils utilisent les mêmes uniforms. Liste complète des uniforms moteur non
annotés qu'ils déclarent :

- `g_ModelMatrix`, `g_ViewProjectionMatrix`, `g_ModelViewProjectionMatrix`, `g_EyePosition` ;
- `g_LightsPosition`, `g_LightsColorRadius`, `g_LightAmbientColor`, `g_LightSkylightColor` ;
- `g_ViewUp`, `g_ViewRight`, `g_Alpha` ;
- `g_TexelSize`, `g_TexelSizeHalf`, `g_Texture0Resolution` ;
- `g_Time`, `g_AudioSpectrum16Left/Right`.

[Observé] `fantasticcar/grid.vert` déclare `g_ModelMatrix` en **`mat3`** (inutilisé). Le téléversement d'un uniform
dont le type déclaré diffère est simplement omis (spec 03 §12.4).

Ajouts au dialecte nécessaires aux shaders 3D. Absents de la spec 03 §3.3, ce sont des faits de format :

| Forme | Sens |
|---|---|
| `CASTU(x)`, `CASTF(x)` | `uint(x)`, `float(x)` |
| `sampler2DComparison` | sampler de comparaison de profondeur (texture d'ombre) |
| `texSample2DCompare(s, uv, ref)` | résultat de la comparaison `ref` ≤ profondeur stockée, filtré. Le shader en lit `.r` : la forme DOIT donner une valeur dont `.r` est ce résultat, par exemple un `vec4` [Inféré] |
| `DECLARE_SAMPLER2D_PARAMETER(n)`, `MAKE_SAMPLER2D_ARGUMENT(n)` | paramètre `sampler2D n` d'une fonction, et l'argument correspondant |
| `texLoad2D(s, uv, res)`, `sampler2DBackBuffer`, `texSample2DBackBuffer(s, uv, res)` | lecture d'un texel `(uv·res.xy)` et échantillonnage d'une copie de cible. Volumétrie seulement, P3 |
| `clip(x)` | abandon du fragment si `x < 0`. Volumétrie seulement |
| `mat4x3`, attributs `uvec4` | types natifs, déjà requis par la spec 08 |
| `gl_InstanceID`, `gl_ViewportIndex` (`shadowcaster*`) | rendu par instance dans 6 viewports. wgpu n'a pas d'index de viewport en sortie de sommet : Lumenwall DOIT rendre chaque viewport dans une passe distincte, avec `gl_InstanceID` remplacé par la constante du viewport, et ignorer `gl_ViewportIndex` (P3) |

### 1.13 Textures des modèles

| Usage | Slot (shaders `generic*` / `*4`) | Format et canaux | Observé |
|---|---|---|---|
| Albédo | 0 | RGBA (DXT1/5, RGBA8888, image embarquée). Défaut [Shader WE] `util/white` | tous |
| Normales | 1 | `dxt5n` / `dxt5n+` (`.tex` format 4) : x dans l'alpha, y dans le vert (§1.11.4). `rg88n`, `rgba8888n` possibles | `arsenal`, `fantasticcar` |
| Lightmap (shader `generic`, combo `LIGHTMAP`) | 2 avec `NORMALMAP`, sinon 1 | RGB, échantillonnée avec le **2e jeu d'UV** (`a_TexCoordVec4.zw`) | `arsenal` |
| Masques PBR (`*4`, combo `PBRMASKS`) | 2 | x = métal, y = rugosité, z = réflexion, a = émissif [Shader WE]. [Doc WE] l'éditeur n'en prend que le rouge, pour l'émissif | — |
| Réflexion | 3 | `_rt_Reflection` (`generic`, matériaux propres) ou `_rt_MipMappedFrameBuffer` (`*3`, `*4`) | `arsenal`, `fantasticcar`, `audiophile` |
| Dégradé de toon (`SHADINGGRADIENT`) | 4 | Défaut `gradient/gradient_toon_smooth` | — |
| Morphing | 5 | `"material":"morph"` | — |
| Atlas d'ombres | 6 | `_rt_shadowAtlas`, comparaison | — |
| Cookie | 7 | `_alias_lightCookie` | — |

[Observé] Aucune cubemap ni carte d'environnement dans le corpus : les ciels sont des maillages (`skybox.mdl`, cube
de côté 2 centré sur l'œil ; sphère `backgroundsphere`). Aucune texture `VOLUME` (spec 01 A.1) n'est liée à un modèle.

### 1.14 Attributs de sommet fournis aux passes de modèle

Pour chaque maillage, le moteur lie **par nom** (spec 03 §3.4) :

| Attribut | Source |
|---|---|
| `a_Position` (vec3) | position |
| `a_Normal` (vec3) | normale ; (0, 0, 0) si absente [Inféré : règle (0, 0, 0, 1) de la spec 03] |
| `a_Tangent4` (vec4) | tangente |
| `a_TexCoord` (vec2) | UV ; avec `0x20`, (u0, v0) |
| `a_TexCoordVec4` (vec4) | avec `0x20`, (u0, v0, u1, v1) ; avec `0x8` seul, (u, v, 0, 1) [Inféré] |
| `a_BlendIndices` (uvec4), `a_BlendWeights` (vec4) | os (§1.10.6) |
| `a_Color4u`, `a_Color` | non fourni (commenté dans `generic4.vert`) ; valeur par défaut |

Tampons statiques : un tampon de sommets et un tampon d'indices par maillage, téléversés une seule fois.
`docs/PERFORMANCE.md` s'applique : aucune allocation ni aucun téléversement par frame pour un modèle immobile.

---

## 2. Éclairage

### 2.1 Ambiante et ciel

- `g_LightAmbientColor` = `general.ambientcolor` ; `g_LightSkylightColor` = `general.skylightcolor`. Valeurs **liées
  en direct** [Inféré : [Doc WE] `IScene.ambientcolor` et `skylightcolor` sont modifiables par script]. Cela amende
  la spec 04 §13.2, qui les fige.
- [Shader WE] Les shaders calculent eux-mêmes l'ambiante d'un sommet de normale monde `n` :
  `A(n) = mix(skylight, ambient, n.y·0,5 + 0,5)`. Le moteur fournit seulement les deux couleurs.

### 2.2 Ensemble des lumières actives

- [Doc WE] « limits the maximum number of lights per scene to four light sources ». Lumenwall DOIT retenir **au plus
  4** lumières : les premières lumières **visibles** dans l'ordre du fichier [Inféré, QO-3D-15].
- Le jeu actif est recalculé quand une visibilité, un type ou le nombre de lumières change (propriété, script). Pas à
  chaque frame : seules leurs positions et couleurs, qui sont des uniforms, changent par frame.
- `general.lightconfig` [Observé] est un **cache de l'éditeur** : nombre de lumières par type (`point`) et nombre
  d'ombrées (`pointshadow`). Lumenwall DEVRAIT recalculer ces nombres à partir des objets, et PEUT comparer avec
  `lightconfig` en diagnostic [Inféré].
- Type effectif : `point` et `lpoint` sont des ponctuelles. Les autres types sont dans §1.6.

### 2.3 Interface héritée : `g_LightsPosition[4]` / `g_LightsColorRadius[4]` (P1)

Utilisée par `generic`, `generic2`, `genericimage2` et le shader `core` de `demon_core`. Pour la lumière active
d'indice `i` (0 à 3, §2.2) :

| Uniform | Valeur |
|---|---|
| `g_LightsPosition[i]` (vec3) | position monde de la lumière (§3.3) |
| `g_LightsColorRadius[i]` (vec4) | `(color.r·I, color.g·I, color.b·I, radius)`, où `I` = `intensity` [Inféré, QO-3D-16] |

- Emplacements inutilisés : tout à zéro [Inféré]. Une atténuation de rayon nul donne 0 (hors fragment exactement
  situé sur la lumière) ; aucune lumière fantôme.
- Les 4 éléments de chaque tableau DOIVENT être téléversés. La spec 03 §12.4 n'en téléverse qu'un pour un tableau de
  vecteurs : c'est à amender.
- Les shaders se chargent des calculs. [Shader WE] `ComputeLightSpecular` et `ComputeLight` (`common_fragment.h`) :
  atténuation `a = saturate((r − d)/r)` appliquée **au carré** au diffus, demi-Lambert piloté par l'uniform `g_Light`
  (matériau `Light`), puissance spéculaire `(1,01 − rugosité)·mix(400, 250, métal)`.
- Vecteur L-1 (§4.5).

### 2.4 Module `LightingV1` (P2)

Le fragment de `generic4`, `foliage4`, `fur4`, `chroma4` et `genericimage4` contient `#require LightingV1`. Il appelle :

`PerformLighting_V1(position_monde, albédo, normale, vue, teinte_spéculaire, f0, rugosité, métal) → vec3`

(signature de la spec 03 §4.2). Le module est **généré par le moteur** : son texte n'est pas dans les assets
[Observé]. Ses uniforms ne sont donc pas visibles des données. Lumenwall DOIT générer un module qui calcule la somme
ci-dessous, au lieu du `vec3(0)` de l'amont. Les noms d'uniforms de ce module sont libres. Lumenwall DEVRAIT les
préfixer pour éviter toute collision avec un shader d'atelier (ex. `g_LumenLight…`).

Point d'insertion : juste avant la directive (spec 03 §4.2). Dans les cinq shaders, `common_pbr_2.h` est inclus
**avant** la directive [Shader WE] : le module PEUT appeler `ComputePBRLightShadow`, `ComputePBRLightShadowInfinite`,
`PointSegmentDelta`, `CalculateProjectedCoords*` et `PerformShadowMapping`.

Pour un fragment en `P`, de normale `N`, avec `V` le vecteur unitaire vers l'œil, et chaque lumière active `l` de
couleur `C_l = color·intensity` [Inféré, QO-3D-16] :

| Type | Contribution [Inféré à partir de `common_pbr_2.h`, `generic3.frag` et `volumetricsfront.frag`] |
|---|---|
| ponctuelle | `ComputePBRLightShadow(N, O_l − P, V, albédo, C_l, radius_l, exponent_l, teinte, f0, rugosité, métal, s_l)` |
| spot | idem, avec `C_l·k_l`, où `k_l = smoothstep(cos θ_ext, cos θ_int, dot(normalise(P − O_l), F_l))`. `F_l` = direction d'émission (§1.6). Avec cookie : `C_l·k_l·cookie(uv projeté)` |
| tube | idem, avec le vecteur `PointSegmentDelta(P, A_l, B_l)` à la place de `O_l − P` |
| directionnelle | `ComputePBRLightShadowInfinite(N, −F_l, V, albédo, C_l, teinte, f0, rugosité, métal, s_l)` |

Avec `s_l = 1` sans ombre (§2.5). Rappel des formules [Shader WE, `common_pbr_2.h`], en mots :

- distance `d = |L|`, `L̂ = L/d`, `H = normalise(V + L̂)` ;
- chute `c = saturate(1 − d/radius)` ; radiance = `C·c^exponent`. La variante HLSL ajoute 1,17549435e-38 à `c`
  avant la puissance ;
- GGX : `NDF = s·r⁴ / (π·((N·H)²·(r⁴ − 1) + 1)²)`, avec `r` = rugosité et `N·H` borné à ≥ 0 ;
- Smith–Schlick : `G = g(max(N·V, 0,001))·g(max(N·L, 0,001))`, `g(x) = x / (x·(1 − k) + k)`,
  `k = (r + 1)²/8` ;
- Fresnel–Schlick : `F = f0 + (1 − f0)·max(1 − H·V, 0,001)⁵` ;
- `NL = max((N·L̂)·s, 0)` ; spéculaire = `NDF·G·F / max(4·max(N·V, 0)·NL, 0,001)` ;
- diffus = `(1 − métal)·(1 − F)` ;
- résultat = `(diffus·albédo/π + spéculaire·teinte)·radiance·NL`.

Le liseré (`RIMLIGHTING`), le dégradé de toon (`GRADIENT_SAMPLER`) et l'éclairage double face
(`DOUBLESIDEDLIGHTING`) modifient `NL` à l'intérieur de ces fonctions : le module n'a rien à en faire.

Le shader combine ensuite lui-même, par `CombineLighting(light, ambient)` : somme simple en LDR, surbrillance bornée
avec `HDR`.

`exponent` d'une lumière héritée (`point` sans clé `exponent`) : 2 [Inféré]. `saturate(1 − d/r)²` est exactement
l'atténuation `a²` de l'interface héritée.

Pour `generic3` (chemin « Deprecated » qui lit directement `g_LPoint_Color[LIGHTS_POINT]`, `g_LPoint_Origin`,
`g_LSpot_Color/Origin/Direction`, `g_LTube_Color/OriginA/OriginB`, `g_LDirectional_Color/Direction`), Lumenwall
DEVRAIT définir les combos `LIGHTS_*` (nombre par type) et téléverser [Inféré d'après l'usage dans le shader] :

- `Color = (rgb, intensité)` ;
- `Origin.xyz` = position, `Origin.w = cos θ_int` ;
- `Direction.xyz` = direction d'émission, `Direction.w = cos θ_ext`.

`SHADERVERSION` choisit la variante (< 62 : pas d'intensité). Sa valeur est inconnue (QO-3D-17). Aucun fond installé
n'utilise `generic3`.

Images 2D éclairées : `genericimage4` avec `LIGHTING` = 1 utilise le même module et les mêmes lumières, en unités de
pixels. `SCENE_ORTHO` = 1 dans une scène orthographique [Shader WE : le vecteur de vue y est fixé à (0, 0, 1)]
[Inféré : combo défini par le moteur quand la scène n'est pas perspective].

### 2.5 Ombres (P3)

Faits de format [Doc WE, Shader WE] :

- [Doc WE] Ombres pour les lumières ponctuelles, spots et directionnelles, si la lumière a `castshadow` et le modèle
  « Cast shadow » (vrai par défaut).
- Atlas `_rt_shadowAtlas`, échantillonné en comparaison (`g_Texture6`, `sampler2DComparison`). `g_Texture6Texel` =
  `(1/largeur, 1/hauteur, largeur, hauteur)` de l'atlas [Inféré d'après l'usage `.xy` comme pas de texel et `.zw`
  comme échelle].
- Chaque lumière ombrée a une région d'atlas `(offset.xy, échelle.zw)` en UV (`atlasTransform`).
  - Spot : coordonnées projetées par sa matrice de vue-projection d'ombre (`CalculateProjectedCoords`, passage NDC →
    UV par `(x·0,5 + 0,5, 0,5 − y·0,5)`).
  - Ponctuelle : 6 faces cubiques rangées en **2 colonnes × 3 lignes** dans sa région. Face +X → (0, 0), −X →
    (1, 0), +Y → (0, 1), −Y → (1, 1), +Z → (0, 2), −Z → (1, 2), en cellules. Les matrices de vue de chaque face et
    la compensation de bord 0,47 / 0,48 / 0,49 selon la qualité sont fixées par `CalculateProjectedCoordsPoint`.
    `projectionInfo` contient les 4 termes z/w de la projection de la face.
  - Directionnelle : jusqu'à 3 cascades (`cascadedistance0..2` [Observé]), `CalculateProjectedCoordsCascades`.
- Filtrage : 1 échantillon (`LIGHTS_SHADOW_MAPPING_QUALITY` = 1) ou 9 échantillons (sinon).
- Passe d'ombre : chaque modèle qui projette une ombre est dessiné dans l'atlas avec le shader de la passe d'ombre de
  son matériau (§1.12, `[PASS] shadow`), une fois par viewport (`g_ViewportViewProjectionMatrices[k]`). Profondeur
  seule. Matériaux avec `ALPHATOCOVERAGE` : test alpha 0,5 sur l'albédo.

Inconnus : taille de l'atlas, réglage de qualité utilisateur, matrices exactes des spots et des cascades, biais
(QO-3D-18). Tant que les ombres ne sont pas implémentées, Lumenwall DOIT laisser `LIGHTS_SHADOW_MAPPING` non défini
(`s_l = 1`). L'atlas RGBA8 vide de la spec 04 §4.3 reste créé pour les références par nom.

### 2.6 Volumétrie (P3)

[Doc WE] Lumières ponctuelles et spots seulement, « very expensive ». [Shader WE] `volumetrics_back/front/combine/
fullscreen/blur_*.json`, tampons `_rt_volumetricsBack`, `_rt_volumetricsSingle`, `_rt_volumetricsLightBuffer(B)`.
`g_RenderVar0` à `g_RenderVar4` regroupent :

| Uniform | Contenu |
|---|---|
| `g_RenderVar0` | transformation d'atlas |
| `g_RenderVar1` | spot : (rayon, cos intérieur, cos extérieur, intensité) |
| `g_RenderVar2` | (origine, densité) |
| `g_RenderVar3` | (avant du spot), ou informations de projection d'une ponctuelle |
| `g_RenderVar4` | (couleur, exposant) |

Contribution : `densité·intensité·(longueur/rayon)·moyenne(chute·cône·ombre)·couleur·0,1`, sur 2 à 64 échantillons
selon la qualité. Non requis par le corpus ; à spécifier si un fond l'utilise (QO-3D-19).

### 2.7 Brouillard (P2)

[Doc WE] Brouillard de distance et de hauteur, réglés dans les options de scène : début, fin, densité de début,
densité de fin, couleur. Désactivable par matériau (combo `FOG`, 1 par défaut). Le brouillard de hauteur est « a
global height setting for the scene ».

[Shader WE, `common_fog.h`] Le moteur définit `FOG_DIST` et/ou `FOG_HEIGHT` quand la scène active le brouillard et que
le combo `FOG` du matériau vaut 1 [Inféré]. Il fournit :

- `g_FogDistanceColor` (vec3), `g_FogDistanceParams` (vec4) ;
- `g_FogHeightColor` (vec3), `g_FogHeightParams` (vec4).

Le shader calcule `f = saturate((x − p.x)/p.y)`, puis `couleur = mix(couleur, couleur_brouillard, p.z + p.w·f²)`, où
`x` est la distance à l'œil (distance) ou l'altitude monde `y` (hauteur). En `ADDITIVE`, l'alpha est atténué.
Correspondance proposée [Inféré] : `p = (début, fin − début, densité_début, densité_fin − densité_début)`. Clés JSON
inconnues (QO-3D-4).

### 2.8 HDR et tonemapping (P3)

- `general.hdr` faux (cas de toutes les scènes installées) : pipeline LDR de la spec 04. Cibles RGBA8 non sRGB,
  valeurs bornées à [0, 1] par la cible, aucun tonemapping, combo `HDR` non défini.
- `general.hdr` vrai [Doc WE « Ultra HDR »] [Inféré] : combo `HDR` = 1 pour les passes de matériau, cible de scène en
  flottant (RGBA16F), bloom HDR (`hdr_downsample` avec `BLOOM`, `hdr_upsample` additif, `combine_hdr`) paramétré par
  `bloomhdrstrength`, `bloomhdrthreshold`, `bloomhdrscatter`, `bloomhdrfeather`, `bloomhdriterations` et `bloomtint`.
  [Shader WE] `combine_hdr` applique une conversion sRGB → linéaire `lin()` et un facteur `g_RenderVar0.x`.
  Composition précise : QO-3D-20.

### 2.9 Uniforms des passes de modèle et de sprite

En plus des uniforms généraux de la spec 03 §12.2 (`g_Time`, spectres audio, pointeur, `g_Texture*Resolution`…),
tous **vifs** et en convention WE (§3.2.2) :

| Nom | Type | Valeur |
|---|---|---|
| `g_ModelMatrix` | mat4 | `M` de l'objet (§3.3) |
| `g_ModelMatrixInverse` | mat4 | `M⁻¹` |
| `g_NormalModelMatrix` | mat3 | `(M₃ₓ₃)⁻ᵀ` [Inféré : `mul(normale, g_NormalModelMatrix)` puis normalisation, identité pour une rotation pure] |
| `g_ViewProjectionMatrix` | mat4 | `P·V` |
| `g_ModelViewProjectionMatrix` | mat4 | `P·V·M` |
| `g_ModelViewProjectionMatrixInverse` | mat4 | inverse |
| `g_EffectModelViewProjectionMatrix`, `g_EffectModelMatrix` | mat4 | comme dans la spec 03 : égaux à MVP et `M` |
| `g_EyePosition` | vec3 | œil effectif (après chemin, tremblement, script) |
| `g_ViewRight`, `g_ViewUp` | vec3 | lignes 0 et 1 de `V` : axes droite et haut de la caméra en monde [Inféré] |
| `g_Screen` | vec3 | `(W, H, W/H)` [Inféré : `normal.y·0,15·g_Screen.z` égalise les décalages en pixels] |
| `g_TexelSize`, `g_TexelSizeHalf` | vec2 | `(1/W, 1/H)` et la moitié [Observé : `bg.vert` en tire le rapport `W/H`] |
| `g_Texture3MipMapInfo` | float | nombre de niveaux de mip de la texture liée au slot 3, moins 1 [Inféré, QO-3D-21] |
| `g_LightAmbientColor`, `g_LightSkylightColor` | vec3 | §2.1 |
| `g_LightsPosition[4]`, `g_LightsColorRadius[4]` | vec3[4], vec4[4] | §2.3 |
| `g_Alpha`, `g_UserAlpha`, `g_Color`, `g_Color4`, `g_Brightness` | — | `alpha`, `color`, `brightness` de l'objet, défauts 1 / blanc / 1 [Inféré] |
| `g_Bones[BONECOUNT]` | mat4x3[] | §1.10.6 |
| `g_ViewportViewProjectionMatrices[6]` | mat4[6] | passe d'ombre (§2.5) |

### 2.10 Combos moteur des passes 3D

| Combo | Valeur |
|---|---|
| `HLSL` | 1 (§0.3) |
| `GLSL` | 1 (spec 03) |
| `HLSL_SM30`, `PLATFORM_ANDROID`, `REVERSEDEPTH`, `MORPHING`, `MORPHING_NORMALS`, `SKINNING_ALPHA` | non définis |
| `SKINNING`, `BONECOUNT` | §1.10.6 |
| `HDR` | §2.8 |
| `FOG_DIST`, `FOG_HEIGHT` | §2.7 |
| `LIGHTS_SHADOW_MAPPING`, `LIGHTS_SHADOW_MAPPING_QUALITY`, `LIGHTS_COOKIE` | §2.5, non définis tant que non implémentés |
| `LIGHTS_POINT`, `LIGHTS_SPOT`, `LIGHTS_TUBE`, `LIGHTS_DIRECTIONAL` | §2.4 |
| `ADDITIVE` | 1 si la passe est en `blending: additive` [Inféré : le shader atténue alors l'alpha par le brouillard] |
| `ALPHATOCOVERAGE` | 1 si la passe demande l'alpha-to-coverage [Doc WE : « Set Blending to Alpha to coverage »]. Valeur JSON de `blending` inconnue (QO-3D-23). Comme `GLSL` = 1, le shader fait alors `discard` sous 0,5 |
| `TEX<n>FORMAT` | §1.11.4 |

---

## 3. Rendu

### 3.1 Cibles, profondeur, formats

- Cible couleur de scène : `_rt_FullFrameBuffer`, `W × H` (§1.1), RGBA8 non sRGB (spec 04 §15). En LDR, rien ne change.
- **Tampon de profondeur** : une attachement de profondeur `W × H` (`Depth32Float` ou `Depth24Plus`), partagé par
  toutes les passes dessinées dans `_rt_FullFrameBuffer` pendant la frame. Il est créé **seulement** si la scène
  contient un objet modèle ou sprite [Inféré]. Une scène 2D sans modèle reste donc sans profondeur (spec 04 §11.2).
- Effacement en début de frame, si `clearenabled` : couleur `(clearcolor, 1)` (spec 04 §5) et profondeur **1,0**.
- Test : `depthtest enabled` → « inférieur ou égal » ; sinon toujours vrai. Écriture selon `depthwrite`.
- Les RT intermédiaires (composites d'image, RT d'effet, bloom) n'ont pas de profondeur. Une passe d'image ou de texte
  dessinée dans `_rt_FullFrameBuffer` utilise le tampon de profondeur si son matériau le demande (ex.
  `composelayer_depthtest.json`, `solidlayer_depthtest.json`). Sa profondeur vient de sa propre position clip.
- Le passage vers une autre convention que celle de WE ne change pas la profondeur : avec les mêmes `n` et `f`, la
  profondeur `(z_gl + w)/(2w)` de la spec 04 §24.2 est identique à `z_WE/w` (§3.2.2). Particules et images peuvent
  donc partager le tampon avec les modèles.

### 3.2 Caméra

#### 3.2.1 Matrice de vue

`V = lookAt(eye, center, up)`, définition de la spec 04 §0.3 (main droite : avant `f`, droite `s = normalise(f × up)`,
haut `u = s × f`).

#### 3.2.2 Projection (convention WE)

Avec `c = 1/tan(fov·π/360)` (fov vertical en degrés), `a = W/H`, `n = nearz` et `f = farz` :

```
P = | c/a  0   0          0          |
    | 0    c   0          0          |
    | 0    0   f/(n − f)  n·f/(n − f) |
    | 0    0  −1          0          |
```

C'est une perspective main droite, profondeur [0, 1], `y_ndc = +1` en haut. `g_ViewProjectionMatrix = P·V`. Vecteurs
VP-1 et VP-2 (§4.5).

Avec `nearz ≤ 0` ou `farz ≤ nearz`, Lumenwall DOIT revenir aux défauts (0,01 / 10000), avec un avertissement
[Inféré].

Une implémentation qui garde la convention de la spec 04 pour une passe 3D doit utiliser `Sc(1, −1, 1)·P_GL` à la
place de `P`. Elle perd alors les adaptations `HLSL` (§0.3) et diverge sur les shaders qui écrivent eux-mêmes leur
position clip (`bg.vert`, `cloudsbg.vert`). C'est **déconseillé**.

#### 3.2.3 Caméra statique et priorité

Sans `paths` : caméra fixe `(eye, center, up)` de `scene.json`. Avec des chemins : la pose vient du chemin courant
(§3.2.4). [Doc WE] `getCameraTransforms`/`setCameraTransforms` : « Do not use this while using a camera path! ». Une
pose écrite par script remplace celle du chemin pour la frame [Inféré].

#### 3.2.4 Lecture des chemins (P1)

[Inféré, QO-3D-5 et QO-3D-6] :

1. Les chemins sont joués **dans l'ordre** du tableau. Chacun est joué une fois, puis vient le suivant. Après le
   dernier, on revient au premier. Un seul chemin se répète.
2. Temps local `τ ∈ [0, duration)`, mesuré sur l'horloge des scènes (`g_Time`, spec 04 §2.1). Le chemin `k` commence
   quand le précédent se termine. Le premier commence à `g_Time = 0`.
3. Instants des images clés : si le dernier `timestamp` `T` est > 0 et différent de `duration`, les instants sont
   **remis à l'échelle** : `t_i' = timestamp_i·duration/T` (cas `demon_core`). Sinon, ils sont pris tels quels.
4. Interpolation **linéaire** de `eye`, `center` et `up` entre les deux images clés qui encadrent `τ`, puis `up`
   renormalisé. Avant la première image clé ou après la dernière : valeur de l'image clé la plus proche. Une seule
   image clé : pose constante.
5. `zoom` est ignoré.

Les poses des images clés ne suivent pas toujours `camera` de `scene.json` (`arsenal` : la caméra statique ne regarde
pas les pistolets). La pose de `scene.json` ne sert qu'en l'absence de chemin.

#### 3.2.5 Fondu de caméra (P2)

[Doc WE] `camerafade` : « Whether the camera will show the fade effect when changing paths ». Proposition [Inféré,
QO-3D-24] : quand `camerafade` est vrai et que la scène a au moins 2 chemins, l'image finale est multipliée par un
facteur `φ` :

- `φ` passe de 1 à 0 pendant les 0,5 dernières secondes d'un chemin ;
- `φ` passe de 0 à 1 pendant les 0,5 premières secondes du suivant ;
- variation linéaire, application après le bloom.

Sur 1 seul chemin : aucun fondu.

#### 3.2.6 Tremblement de caméra (P2)

[Doc WE] `camerashake` (on/off), `camerashakespeed` (« base speed »), `camerashakeamplitude` (« magnitude »),
`camerashakeroughness` (« how rough »). [Observé] `ricepod` : vrai, 0,01, 0,1, 5. La formule de WE est inconnue
(QO-3D-25).

Proposition provisoire, déterministe et indépendante de la cadence [Inféré] : on ajoute à `eye` **et** à `center` le
même décalage `δ(t) = amplitude·(n₁(t·speed), n₂(t·speed), n₃(t·speed))`, où :

- `nᵢ` est un bruit de Perlin 1D (spec 05 §9.3, sur une coupe) de graine différente par axe ;
- `n₂` est complété d'une octave à fréquence double pondérée par `roughness`.

Marquer `// parity:` à l'implémentation. Ne pas recalculer `V` si `camerashake` est faux.

#### 3.2.7 Parallaxe en 3D

[Observé] Les 8 scènes ont `cameraparallax` faux ou absent ; certaines portent `parallaxDepth` (1, 1) sur leurs
modèles. La formule 3D de WE est inconnue. Lumenwall DOIT ignorer `parallaxDepth` des modèles quand `cameraparallax`
est faux. Quand il est vrai dans une scène perspective : QO-3D-26. Il n'y a pas de proposition normative avant
d'avoir un échantillon : journaliser une fois et ne rien décaler.

### 3.3 Transformations des objets 3D

- Matrice locale `L = T(origin)·Rz(a.z)·Ry(a.y)·Rx(a.x)·S(scale)`, `a` = `angles` en radians. Rotations main droite :
  le point subit l'échelle, puis Rx, Ry, Rz, puis la translation [Inféré, QO-3D-27]. C'est la même composition que
  celle des particules de la spec 05 §2.3, une fois retirée l'inversion d'axe y propre au repère 2D.
  [Observé] `neongrid` (`angles` (0, 3,142, 0)) et les deux `curve` de `dna_fragment` (angles x et z ≈ ±π)
  n'imposent pas l'ordre : seuls Ry et la paire Rx, Rz y comptent.
- Hiérarchie : `M = M_parent·L` pour un parent modèle, lumière, sprite ou générique [Inféré] (spec 02 §13.3). Un
  parent image : QO-3D-28.
- `g_NormalModelMatrix` : §2.9.
- `M` n'est recalculée que si `origin`, `angles`, `scale` ou le parent change.

### 3.4 Déroulé d'une frame d'une scène perspective

Après les étapes 1 à 4 de la spec 04 §5 (pointeur, parallaxe 2D, script, textures vidéo) :

1. **Caméra** : pose (§3.2.3 à §3.2.6), puis `V`, `P·V`, `g_EyePosition`, `g_ViewRight`, `g_ViewUp`.
2. **Lumières** : jeu actif (§2.2), positions monde, uniforms §2.3 et §2.4.
3. **Ombres** (P3) : si au moins une lumière ombrée, remplir `_rt_shadowAtlas` (§2.5).
4. **Réflexion** : si au moins un matériau chargé lie `_rt_Reflection`, remplir cette RT (§3.7).
5. **Passe principale** dans `_rt_FullFrameBuffer` : effacement couleur et profondeur (§3.1), puis objets dans
   l'**ordre de rendu de la spec 04 §4.6** (ordre du fichier, dépendances d'abord). Chaque objet garde son rendu
   propre : modèles §3.5, sprites §3.5.3, images et effets (spec 04, §3.9), particules (§3.9), texte, sons. Les
   lumières ne dessinent rien.
6. **Tri des transparents** si `transparentsorting` (§3.6).
7. **Bloom** (§3.10), en dernier, comme dans la spec 04 §4.6.
8. **Fondu de caméra** (§3.2.5).
9. Composition finale (spec 04 §21).

### 3.5 Dessin d'un modèle

#### 3.5.1 Maillages et passes

Pour chaque maillage, dans l'ordre du fichier, et pour chaque passe de son matériau, dans l'ordre (le plus souvent
une seule) :

1. Programme : shader de la passe, combos (spec 03 §5 et §2.10), uniforms (§2.9), textures (spec 03 §8). La
   texture 0 vient du matériau : il n'y a pas d'`instance` d'image.
2. États : §1.11.2 (défauts des modèles), blending de la spec 04 §14.
3. Culling `normal` : élimination des faces **arrière**. La face avant est celle dont les sommets, dans l'ordre des
   indices, tournent dans le **sens trigonométrique à l'écran**, en convention WE (`y` vers le haut). Avec wgpu et la
   convention WE : `FrontFace::Ccw`, `Face::Back`. `nocull` : aucune élimination.
4. Dessin indexé `u16` en liste de triangles, de toute la plage du maillage.

Un objet modèle invisible ne dessine rien et ne participe pas à la réflexion. Un modèle dont le `.mdl` a échoué, ou
dont un matériau a échoué, n'est pas dessiné, avec un avertissement. Les autres maillages du même modèle restent
dessinés [Inféré]. Contrairement aux images, cela ne fait **pas** échouer la scène (spec 02 Q31).

#### 3.5.2 Écriture de l'alpha

Les passes de modèle écrivent les 4 canaux (le shader `car` écrit `alpha = 0,4` en `blending normal`). La sortie
finale étant présentée opaque (spec 04 §11.6, Q11), seul un shader qui relit l'alpha de `_rt_FullFrameBuffer` le voit
(QO-3D-29).

#### 3.5.3 Sprite

Géométrie du §1.7. `g_ModelViewProjectionMatrix = P·V·M`, avec `M` le §3.3 appliqué à `origin`, `angles` et `scale`
du sprite. États : matériau, avec les défauts §1.11.2. [Observé] `sunsprite.json` : additif, profondeur désactivée.

### 3.6 Tri des transparents

- `transparentsorting` absent ou faux : ordre du fichier, sans tri. C'est le cas des 8 scènes installées.
- Vrai [Inféré, QO-3D-30] : les objets modèle dont **toutes** les passes sont en `translucent` ou `additive` sont
  retirés de l'ordre. Ils sont dessinés après les autres objets de la passe principale, mais avant le bloom, triés
  par distance décroissante entre l'œil et le centre de leur boîte englobante transformée. À distance égale, l'ordre
  du fichier départage.

### 3.7 Réflexion plane `_rt_Reflection` (P1)

[Observé] `fantasticcar` : `dome` et `body` portent `reflected: true`. Le matériau de `grid` lie `_rt_Reflection`.
`preview.jpg` montre la voiture reflétée **sous** elle-même, roues contre roues, déformée par le relief de la grille.
C'est une réflexion plane par rapport au sol `y = 0`, lue à la **même position écran**. `audiophile` fait de même
(barres reflétées, grille en `y = 0`).

Règle [Inféré, QO-3D-31] :

1. `_rt_Reflection` : RT couleur RGBA8 de taille `W × H`, échantillonnage linéaire et clamp-to-edge, sans mip. Elle
   a sa propre profondeur, effacée à 1. Elle est créée seulement si un matériau chargé la référence. Elle est
   enregistrée dans les RT globales de la scène **avant** la création des objets, pour que les noms se résolvent
   (spec 04 §4.3).
2. À chaque frame, avant la passe principale :
   - effacement à `(clearcolor, 1)` ;
   - dessin, dans l'ordre de rendu, des objets **visibles** marqués `reflected`, avec la vue miroir
     `V_r = V·Sc(1, −1, 1)`. `g_EyePosition` devient `(eye.x, −eye.y, eye.z)`, et `g_ViewProjectionMatrix` et MVP sont
     recalculés avec `V_r` ;
   - comme la symétrie inverse l'orientation, le culling s'inverse : `FrontFace::Cw`.

   Mêmes matériaux, mêmes états sinon.
3. Les shaders lisent `_rt_Reflection` aux coordonnées écran qu'ils calculent eux-mêmes. Exemples :
   `(clip.xy/clip.w)·0,5 + 0,5` plus le relief (`grid`), ou plus `normale.xy·0,01` (`generic`). Ils n'ont pas de
   branche `HLSL` : en convention WE, `v` y vaut 1 en haut de l'écran. La RT DOIT donc être lue de façon que la
   réflexion apparaisse à l'endroit du `preview.jpg` (voiture inversée sous la voiture). C'est l'objet du fixture
   F-3D-R2 et de QO-3D-31.
4. [Observé] `arsenal` lie `_rt_Reflection` (combo `reflection` de `planks`) sans aucun objet `reflected`. La RT
   contient alors seulement la couleur d'effacement (noir dans `arsenal`) : le terme ajouté est nul.

### 3.8 `_rt_MipMappedFrameBuffer` (P2)

[Shader WE] `generic3`, `generic4`, `foliage4` et `genericimage4` avec `REFLECTION` lisent `g_Texture3` =
`_rt_MipMappedFrameBuffer` avec un niveau de détail `rugosité·g_Texture3MipMapInfo`. C'est une réflexion en espace
écran sur l'image déjà dessinée. Pour ces passes, Lumenwall DOIT fournir :

- une copie de `_rt_FullFrameBuffer` prise juste avant la passe ;
- avec sa **chaîne complète de mips**, régénérée à chaque copie.

Cela amende la spec 04 §3 (« sans niveaux de mipmap ») [Inféré] (QO-3D-21). La copie évite aussi la lecture-écriture
simultanée (spec 04 §24.6). Elle n'est faite que si une telle passe est visible dans la frame.

### 3.9 Interaction avec les calques 2D, les effets et les particules

- **Images dans une scène perspective** [Observé : `neon_sunset`, deux images `fullscreen` avec `copybackground`] :
  - les objets image passent par la chaîne de la spec 04 et se composent dans `_rt_FullFrameBuffer` à leur rang dans
    l'ordre de rendu ;
  - `_rt_FullFrameBuffer` contient à ce moment tout ce qui est déjà dessiné, modèles compris. Le calque `filmgrain`
    placé en dernier traite donc l'image 3D ;
  - une image `fullscreen` couvre exactement la cible. [Observé] `cloudsbg.vert` écrit `gl_Position =
    vec4(a_Position, 1)` sans matrice. Dans une scène perspective, la passe de base d'une image `fullscreen` DOIT
    donc recevoir un quadrilatère en **espace clip** : `a_Position` ∈ {−1, +1}² et `z = 0`. `a_TexCoord` vaut
    (0, 0) au coin **haut-gauche** de la cible, et les matrices moteur de la passe sont l'identité [Inféré]. Le coin
    haut-gauche se trouve en `(−1, +1)` en convention WE et en `(−1, −1)` en convention de la spec 04 ; le résultat
    visible est le même ;
  - images non `fullscreen` dans une scène perspective : absentes du corpus, placement inconnu (QO-3D-32).
- **Effets** : ils ne s'appliquent qu'aux objets image (spec 04 §9). Un modèle n'a pas d'effet. Pour traiter la 3D,
  WE place un calque `fullscreen` avec `copybackground` après les modèles [Observé].
- **Particules dans une scène perspective** [Observé : `demon_core`, `dna_fragment`, `neon_sunset`, `ricepod`] :
  - le système vit dans l'espace monde de la scène : origine `origin` telle quelle, sans le recentrage `−W/2`,
    `H/2 − y` de la spec 05 §2.2 ;
  - sa matrice modèle est celle du §3.3 ;
  - VP = caméra de la scène (§3.2), dans la convention qu'utilise le module de particules ;
  - pas d'œil fixe en (0, 0, 1000) : `g_EyePosition`, `g_ViewUp` et `g_ViewRight` de la caméra ;
  - test de profondeur selon le matériau [Observé : `ricepod/particle.json` `depthtest enabled`, `depthwrite
    disabled`] ;
  - les inversions de l'axe Y de la spec 05 §2.2 ne s'appliquent **pas** dans une scène perspective [Inféré, QO-3D-33]
    (`dna_fragment` : émetteur à `y = −0,4`, vitesse `+y` : les particules montent).
- **Texte** : dessiné à son rang, sans profondeur [Inféré].
- **Parallaxe** : §3.2.7.

### 3.10 Bloom

Le bloom de la spec 04 §17 s'applique **sans changement** à la fin de la passe principale, sur `_rt_FullFrameBuffer`
(modèles, images, particules), avec les tailles dérivées de `W × H`.

Valeurs par défaut quand `bloom` est vrai mais que `bloomstrength` ou `bloomthreshold` manque (`arsenal`, `demon_core`,
`dna_fragment`) : WE prend **2,0** et **0,65** [Inféré : gabarits `modeleditor` et `collisionmodel` ; 170 des 173
`scene.json` installés qui fixent ces clés ont ces valeurs]. Lumenwall DEVRAIT les appliquer (règle 5) plutôt que 0 / 0 (spec 02 Q21, spec 04 §17).

### 3.11 Anticrénelage

- [Observé] Aucune clé MSAA dans les scènes : chez WE, c'est un réglage de qualité de l'application.
- Lumenwall NE DOIT PAS activer de MSAA par défaut (`docs/research/render.md`). Il PEUT proposer une option globale
  MSAA ×4 limitée à la passe principale des scènes 3D, avec résolution avant le bloom. Ce n'est pas un comportement de
  parité.
- Alpha-to-coverage : §2.10. Sans MSAA, le `discard` sous 0,5 du chemin `GLSL` sert de test alpha.

### 3.12 Modèles dans une scène 2D (P2)

[Observé] `retro` (modèle `bgfade` dans une scène orthographique, shader en espace clip) et l'aperçu `collisionmodel`
(sphère `generic4`, `origin` (128, 128, 0) en pixels, échelle 0,678, boîte ±100).

- Unités : pixels [Doc WE]. Repère de scène WE (origine en bas à gauche, y vers le haut).
- Sans `perspective` : `VP` = projection orthographique de la scène. Pour rester dans le repère WE, on ramène le point
  au centre de la projection sans retourner y : `VP = ortho_WE·T(−W/2, −H/2, 0)`, avec `ortho_WE` une orthographique
  ±W/2 × ±H/2, y vers le haut. Sa profondeur [0, 1] est répartie linéairement sur `z ∈ [−farz, +farz]` autour du plan
  `z = 0` [Inféré, QO-3D-3]. La sphère de `collisionmodel` (z de −68 à +68 autour de l'œil en z = 0) exclut un plan
  proche positif. Profondeur partagée (§3.1). Les objets image restent sur leur chaîne 2D (spec 04).
- `perspective: true` [Doc WE, `createLayer({perspective: true})` : « enables 3D rendering in a 2D scene », champ
  `FOV` de la scène] : perspective `P(fov', W/H, nearz, farz)` avec `fov'` = `perspectiveoverridefov` si présent,
  sinon `fov`. La pose de caméra qui fait coïncider le plan `z = 0` avec la projection 2D est inconnue (QO-3D-3).
- `SCENE_ORTHO` = 1 pour les passes de modèle d'une scène 2D (§2.4).

---

## 4. Fixtures et vecteurs de test

### 4.1 F-3D1 — `MDLV0014`, quadrilatère, `vf = 0x9` (143 octets)

Fabriqué pour cette spec (aucune donnée WE). SHA-256 :
`32ba848b54f94b3af6f6dd56d9ce1a898df1502fd1cea2a9bc390cd04c4c3076`.

```
00000000: 4d44 4c56 3030 3134 0009 0000 0001 0000  MDLV0014........
00000010: 0001 0000 006d 6174 6572 6961 6c73 2f6d  .....materials/m
00000020: 2e6a 736f 6e00 0000 0000 5000 0000 0000  .json.....P.....
00000030: 80bf 0000 80bf 0000 0000 0000 0000 0000  ................
00000040: 803f 0000 803f 0000 80bf 0000 0000 0000  .?...?..........
00000050: 803f 0000 803f 0000 803f 0000 803f 0000  .?...?...?...?..
00000060: 0000 0000 803f 0000 0000 0000 80bf 0000  .....?..........
00000070: 803f 0000 0000 0000 0000 0000 0000 0c00  .?..............
00000080: 0000 0000 0100 0200 0000 0200 0300 00    ...............
```

Attendu : `S = 1`, 1 maillage, matériau `materials/m.json`, 4 sommets de 20 o :

| Sommet | Position | UV |
|---|---|---|
| 0 | (−1, −1, 0) | (0, 1) |
| 1 | (1, −1, 0) | (1, 1) |
| 2 | (1, 1, 0) | (1, 0) |
| 3 | (−1, 1, 0) | (0, 0) |

Indices (0, 1, 2, 0, 2, 3). Fin de lecture en 142, octet `0x00` final. Les deux triangles tournent dans le sens
trigonométrique vus depuis +Z : face avant vers la caméra par défaut (œil en +Z, visée −Z).

### 4.2 F-3D2 — `MDLV0004`, 2 skins, triangle, `vf = 0xB` (169 octets)

SHA-256 : `5731638a30d8028e7c4d3e930b7114c5a4709f88281702af8a72db6dc4ae9cfc`.

```
00000000: 4d44 4c56 3030 3034 000b 0000 0002 0000  MDLV0004........
00000010: 0001 0000 006d 6174 6572 6961 6c73 2f61  .....materials/a
00000020: 2e6a 736f 6e00 6d61 7465 7269 616c 732f  .json.materials/
00000030: 622e 6a73 6f6e 0000 0000 0060 0000 0000  b.json.....`....
00000040: 0000 0000 0000 0000 0000 0000 0000 0000  ................
00000050: 0000 0000 0080 3f00 0000 0000 0080 3f00  ......?.......?.
00000060: 0080 3f00 0000 0000 0000 0000 0000 0000  ..?.............
00000070: 0000 0000 0080 3f00 0080 3f00 0080 3f00  ......?...?...?.
00000080: 0000 0000 0080 3f00 0000 0000 0000 0000  ......?.........
00000090: 0000 0000 0080 3f00 0000 0000 0000 0006  ......?.........
000000a0: 0000 0000 0001 0002 00                   .........
```

Attendu : `S = 2`. Matériaux `materials/a.json` (skin 0) et `materials/b.json` (skin 1). Sommets :

| Sommet | Position | Normale | UV |
|---|---|---|---|
| 0 | (0, 0, 0) | (0, 0, 1) | (0, 1) |
| 1 | (1, 0, 0) | (0, 0, 1) | (1, 1) |
| 2 | (0, 1, 0) | (0, 0, 1) | (0, 0) |

Indices (0, 1, 2). Fin en 169 = taille : **pas** d'octet terminal. `skin: 5` donne `materials/a.json`, avec un
avertissement (§1.11.1).

### 4.3 F-3D3 — `MDLV0017`, 2 maillages, `vf = 0x27` (484 octets)

SHA-256 : `6797460743e342b336857f771401d87a24bf3ad5861f6abd4783029f51b396a3`. Construction : en-tête `MDLV0017`,
drapeaux `0x27`, `S = 1`, `nm = 2`, puis deux maillages et l'octet `0x00` final. Maillage `k` (`k` = 0, 1 ;
`dz` = 0 puis −1 ; matériau `materials/a.json` puis `materials/b.json`) :

- `u32` 0 ;
- boîte `(0, 0, dz, 1, 1, dz)` ;
- `vf` = `0x27`, `vb` = 168 ;
- 3 sommets de 56 o : position, normale (0, 0, 1), tangente, UV4 :

| Sommet | Position | Tangente | UV4 (u0, v0, u1, v1) |
|---|---|---|---|
| 0 | (0, 0, dz) | (1, 0, 0, 1) | (0, 1, 0,25, 0,75) |
| 1 | (1, 0, dz) | (1, 0, 0, 1) | (1, 1, 0,75, 0,75) |
| 2 | (0, 1, dz) | (1, 0, 0, **−1**) | (0, 0, 0,25, 0,25) |

- `ib` = 6, indices (0, 1, 2).

Attendu : `a_TexCoord` du sommet 1 = (1, 1) ; `a_TexCoordVec4` = (1, 1, 0,75, 0,75). Bitangente du sommet 2 =
`(n × t)·w` = (0, −1, 0). Fin en 483, octet `0x00`.

### 4.4 Variantes négatives (partir de F-3D1)

| Modification | Attendu |
|---|---|
| octet 0x09 (drapeaux) ← `0x19` (bit `0x10` inconnu) | refus « format de sommet inconnu », modèle non dessiné, pas d'arrêt |
| octet 0x09 ← `0x29` (`0x8` et `0x20`) | refus (§1.10.3) |
| `vb` (0x2A) ← 81 | refus « taille de sommets » |
| dernier indice (0x8C) ← 4 | refus « indice hors bornes » |
| `S` (0x0D) ← 0 | refus |
| fichier tronqué à 100 octets | refus « fin de fichier » |
| octet terminal (0x8E) ← `0x4D` suivi de rien | fin de chaînage sans section, avec avertissement (spec 08 §3, règle 2) ; modèle **accepté** |

### 4.5 Vecteurs (tolérance 1e-5 sauf mention)

| Id | Entrée | Attendu |
|---|---|---|
| VP-1 | œil (0, 0, 0), visée (0, 0, −1), haut (0, 1, 0), fov 50, `a` = 16/9, `n` = 0,01, `f` = 10000 | `P` : `P00` = 1,206285, `P11` = 2,144507, `P22` = −1,000001, `P23` = −0,0100000, `P32` = −1 |
| VP-2 | même caméra, points (0, 0,5, −2), (1, 0, −4), (0, 0, −0,01), (0, 0, −10000) | NDC (0, 0,536127, 0,995001), (0,301571, 0, 0,997501), z = 0 au plan proche, z = 1 au plan lointain. Le premier point, `y_ndc > 0`, est dans la moitié **haute** de l'image : ligne `(1 − 0,536127)/2·1080 = 250,49` sur 1080 |
| M-1 | `origin` (1, 2, 3), `angles` (0, π/2, 0), `scale` (2, 2, 2), sommet (1, 0, 0) | (1, 2, 1) (indépendant de l'ordre des rotations) |
| M-2 (provisoire, QO-3D-27) | `angles` (π/2, 0, π/2), sommet (0, 1, 0) | (0, 0, 1) avec la composition §3.3. (−1, 0, 0) avec l'ordre inverse : ce test DOIT être revu si QO-3D-27 est tranchée autrement |
| N-1 | `scale` (1, 2, 1), normale (1, 1, 0) | `g_NormalModelMatrix·n` = (1, 0,5, 0), normalisée (0,894427, 0,447214, 0) |
| L-1 | `arsenal`, lumière `id` 3 : `color` (0,890, 0,690, 0,086), `intensity` 0,18, `radius` 22,85, 2e lumière visible | `g_LightsColorRadius[1]` = (0,1602, 0,1242, 0,01548, 22,85) ; `g_LightsPosition[1]` = (−1,105, 4,002, 0,347) ; emplacements 2 et 3 nuls |
| R-1 | réflexion, point monde (1, 2, 3), œil (0, 0,1, 3,3) | point dans la vue miroir : `V·(1, −2, 3)` ; `g_EyePosition` réfléchi = (0, −0,1, 3,3) |
| C-1 | `arsenal`, chemin 0 : `duration` 30, œil (−3,544, 2,168, 2,274) en 0 et (−2,618, 1,308, 2,399) en 30, τ = 15 | œil (−3,081, 1,738, 2,3365) |
| C-2 | `demon_core`, chemin 0 : `duration` 300, instants 0 et 40, τ = 150 | mi-parcours entre les deux images clés (remise à l'échelle §3.2.4) |
| C-3 | 2 chemins de 30 s et 40 s, `g_Time` = 75 | chemin 0 (cycle de 70 s), τ = 5 |
| S-1 | cible 1920 × 1080 | `g_Screen` = (1920, 1080, 1,777778) ; `g_TexelSize` = (1/1920, 1/1080) |

### 4.6 F-3D-O — orientation (exigence DOIT du §0.3)

Scène perspective minimale fabriquée : un objet modèle F-3D1, matériau d'une passe, `depthtest disabled`,
`cullmode nocull`, shader de test. Le sommet écrit `gl_Position = vec4(a_TexCoord·2 − 1, 0,5, 1)` et passe
`a_TexCoord`. Le fragment écrit `(v, 0, 0, 1)`.

Attendu, comme chez WE (`dna_fragment`, §0.3) : la **première ligne** de la sortie a un rouge proche de 1, et la
dernière un rouge proche de 0.

Variante avec matrices : le même quad dessiné par MVP sous la caméra VP-1, à `z = −3`. Le sommet (−1, 1, 0) (UV
(0, 0)) DOIT apparaître en haut à gauche.

### 4.7 Données réelles (tests `#[ignore]`, `LUMENWALL_WE_ASSETS`)

Lecture structurée des 29 `.mdl` du dossier d'installation (chemins relatifs à `wallpaper_engine/`) :

| Fichier | Version | `S` | `vf` | Sommets par maillage | Indices par maillage | Fin |
|---|---|---|---|---|---|---|
| `assets/models/editor/camera/camera.mdl` | 0017 | 1 | `0xF` | 692 | 1671 | `0x00` |
| `assets/scenes/particleelementpreviews/collisionmodel/models/sphere/sphere.mdl` | 0023 | 1 | `0xF` | 559 | 2880 | `0x00` |
| `projects/defaultprojects/arsenal/models/pistols/pistols.mdl` | 0014 | 1 | `0x27` | 1423, 8, 4698, 382, 260, 2566 | 4041, 12, 11292, 900, 540, 6720 | `0x00` |
| `audiophile/models/audiophile/bars.mdl` | 0004 | 1 | `0xB` | 384 | 576 | EOF |
| `audiophile/models/audiophile/flow.mdl` | 0004 | 1 | `0x9` | 163 | 600 | EOF |
| `audiophile/models/audiophile/glow.mdl` | 0004 | 1 | `0x9` | 4 | 6 | EOF |
| `audiophile/models/grid/grid.mdl` | 0004 | **2** | `0x9` | 4 | 6 | EOF |
| `demon_core/models/backgroundsphere/backgroundsphere.mdl` | 0014 | 1 | `0xB` | 1087 | 5952 | `0x00` |
| `demon_core/models/core/core.mdl` | 0014 | 1 | `0xB` | 970 | 1440 | `0x00` |
| `dna_fragment/models/bg/bg.mdl` | 0004 | 1 | `0xB` | 4 | 6 | EOF |
| `dna_fragment/models/bg/bgfade.mdl` | 0004 | 1 | `0x9` | 4 | 6 | EOF |
| `dna_fragment/models/curve/curve.mdl` | 0004 | 1 | `0xB` | 4 | 6 | EOF |
| `dna_fragment/models/dna/dna.mdl` | 0004 | 1 | `0xB` | 2604 | 11472 | EOF |
| `fantasticcar/models/car/body.mdl` | 0014 | 1 | `0xF` | 1782, 103, 1824, 2181, 42, 1104 | 4908, 318, 4182, 6315, 120, 2880 | `0x00` |
| `fantasticcar/models/dome/dome.mdl` | 0014 | 1 | `0x9` | 236 | 600 | `0x00` |
| `fantasticcar/models/grid/grid.mdl` | 0014 | 1 | `0x9` | 4 | 6 | `0x00` |
| `fantasticcar/models/util/shadow.mdl` | 0014 | 1 | `0x9` | 90 | 210 | `0x00` |
| `neon_sunset/models/neongrid/neongrid.mdl` | 0014 | 1 | `0xF` | 2601 | 15000 | `0x00` |
| `neon_sunset/models/neonsun/neonsun.mdl` | 0014 | 1 | `0xF` | 4 | 6 | `0x00` |
| `retro/models/bgfade/bgfade.mdl` | 0014 | 1 | `0x9` | 4 | 6 | `0x00` |
| `ricepod/models/ricepod/jet.mdl` | 0014 | 1 | `0x9` | 60 | 168 | `0x00` |
| `ricepod/models/ricepod/orbitaleffects.mdl` | 0014 | 1 | `0x9` | 62, 55, 102 | 180, 174, 360 | `0x00` |
| `ricepod/models/ricepod/ricepod.mdl` | 0014 | 1 | `0xB` | 682, 159, 10995 | 1725, 414, 24999 | `0x00` |
| `ricepod/models/ricepod/skybox.mdl` | 0014 | 1 | `0x9` | 24 | 36 | `0x00` |
| `techno/models/techno/glow.mdl` | 0023 | 1 | `0x9` | 4 | 6 | `0x00` |
| `techno/models/techno/orbitsmall.mdl` | 0023 | 1 | `0x9` | 66, 66, 66, 99 | 192, 192, 192, 384 | `0x00` |
| `techno/models/techno/rays.mdl` | 0023 | 1 | `0x9` | 66 | 192 | `0x00` |
| `techno/models/techno/sphere.mdl` | 0014 | 1 | `0xB` | 417 | 2400 | `0x00` |

Contrôles complémentaires :

- **F-3D-R1** (orientation des faces) : pour chaque maillage à normales, plus de 99 % des triangles ont un produit
  `(b − a) × (c − a)` dans le sens de la somme des normales.
- **F-3D-R1** (sphère) : `backgroundsphere` a 0 normale extérieure sur 1087 (`n·p < 0` pour tous les sommets).
- **F-3D-R1** (UV) : `neonsun.mdl`, sommet (−0,5, 0,5, 0) → UV (0, 0) et sommet (−0,5, −0,5, 0) → UV (0, 1).
- **F-3D-R2** (réflexion, comparaison visuelle manuelle) : `fantasticcar` rendu à la pose de `scene.json`. Le reflet
  des roues avant DOIT se trouver sous les roues, inversé verticalement, comme sur `preview.jpg`.
- **F-3D-R3** (classification) : chaque scène 3D du corpus crée le nombre attendu d'objets modèle (1, 4, 2, 5, 4, 2,
  6, 4 pour `arsenal`, `audiophile`, `demon_core`, `dna_fragment`, `fantasticcar`, `neon_sunset`, `ricepod`,
  `techno`), sans échec de scène. `audiophile` et `techno` sont chargés malgré l'absence de `type` et d'`id`.

---

## 5. Questions ouvertes

- **QO-3D-1 [Inféré]** — `project.json` sans `type` : WE suppose-t-il « scene » d'après l'extension de `file`, ou
  d'après la présence d'objets ? Seuls deux projets officiels de 2016 sont concernés.
- **QO-3D-2 [Inféré]** — `fov` vertical ou horizontal ? Les aperçus sont carrés et ne départagent pas. Sur un écran
  ultra-large, le résultat change fortement. Il faut une capture WE non carrée de `neon_sunset`.
- **QO-3D-3 [Doc WE / Observé]** — Perspective dans une scène 2D (`perspective` sur un calque, `perspectiveoverridefov`) :
  position de l'œil (distance telle que le plan `z = 0` coïncide avec la projection 2D ?) et rôle exact de
  `perspectiveoverridefov` (particules du bit 4, calques texte ou image `perspective`, modèles).
- **QO-3D-4 [Doc WE]** — Brouillard : noms des clés de `general` et correspondance avec `g_FogDistanceParams` et
  `g_FogHeightParams` (§2.7). Aucune scène installée n'en a.
- **QO-3D-5 [Observé]** — `demon_core` : `duration` ≫ dernier `timestamp`. Remise à l'échelle choisie (mouvement lent
  sur toute la durée) ; l'autre lecture est un mouvement en 40 s suivi d'une immobilité de 260 s. Une capture vidéo
  de WE permettrait de trancher.
- **QO-3D-6 [Doc WE]** — Clés JSON des modes *Single*, *Loop*, *Mirror* et de l'ordre aléatoire des chemins. Forme
  d'interpolation (linéaire, Catmull-Rom, avec lissage d'entrée et de sortie ?).
- **QO-3D-7 [Inféré]** — Chaînes de type de lumière autres que `point` et `lpoint`. Différence de sens entre `point`
  (2016) et `lpoint`. Clés des cônes de spot, extrémités de tube, cookie.
- **QO-3D-8 [Inféré]** — Valeur par défaut de `exponent` (2 choisi, pour retrouver l'atténuation héritée `a²`).
- **QO-3D-9 [Observé]** — Format de sommet portant à la fois `0x8` et `0x20`, et autres bits possibles (`0x10`,
  couleur de sommet `IModelData.COLOR` ?). Aucun `.mdl` ne les contient.
- **QO-3D-10 [Inféré]** — Versions `MDLV` non observées (`0005`–`0013`, `0015`, `0016`, `0018`–`0022`) : la règle des
  seuils 17 et 23 est une extrapolation.
- **QO-3D-11 [Doc WE]** — Modèles 3D animés : rotations d'os sur 3 axes, `rootmotion` (`IModelLayer`), calques
  d'animation des modèles. Aucun échantillon disponible.
- **QO-3D-12 [Observé]** — Table `skins` du compagnon contre liste de matériaux du `.mdl` : laquelle fait foi si elles
  divergent ?
- **QO-3D-13 [Observé]** — `depthwriting` et `depthtesting` (`ricepod`) : alias reconnus par WE ou clés ignorées ? Le
  choix des alias suit l'intention de l'auteur.
- **QO-3D-14 [Inféré]** — `formatcombo` : WE définit-il `TEX<n>FORMAT` pour tous les slots liés ou seulement pour les
  samplers annotés ? Il y a une différence visible sur les normales DXT5 (biais 0,965 contre 1,0).
- **QO-3D-15 [Doc WE]** — Avec plus de 4 lumières, lesquelles WE retient-il (ordre, distance, type) ? La limite de 4
  vaut-elle aussi pour l'interface héritée ?
- **QO-3D-16 [Inféré]** — Couleur effective `color·intensity`, ou `color·intensity²` comme dans `generic3` (variante
  `SHADERVERSION ≥ 62`) ? Une capture de `demon_core` ou d'`arsenal` permettrait de calibrer.
- **QO-3D-17 [Inféré]** — Valeur de `SHADERVERSION` fournie par WE.
- **QO-3D-18 [Doc WE / Shader WE]** — Ombres : taille et partage de l'atlas, qualité, matrices des spots et des
  cascades, biais, `REVERSEDEPTH`.
- **QO-3D-19 [Shader WE]** — Volumétrie : ordre des passes, tailles des tampons, sens de `g_AltModelMatrix` et de
  `g_AltViewProjectionMatrix`.
- **QO-3D-20 [Shader WE]** — HDR : format de la cible, contenu de `g_RenderVar0` dans `hdr_downsample` et
  `combine_hdr`, nombre d'itérations, conversion sRGB finale.
- **QO-3D-21 [Inféré]** — `g_Texture3MipMapInfo` : nombre de niveaux moins 1, ou nombre de niveaux ? Fréquence de
  régénération des mips de `_rt_MipMappedFrameBuffer` : avant chaque passe qui la lit, ou une fois par frame ?
- **QO-3D-22 [Spec 03]** — `ddy` en convention WE : confirmer qu'aucun shader 3D du corpus ne dépend du signe. Aucun
  shader des 8 projets n'utilise `ddy`.
- **QO-3D-23 [Doc WE]** — Valeur JSON de `blending` pour « Alpha to coverage » et effet sans MSAA.
- **QO-3D-24 [Doc WE]** — Fondu de caméra : durée, couleur, courbe.
- **QO-3D-25 [Doc WE]** — Formule du tremblement de caméra.
- **QO-3D-26 [Inféré]** — Parallaxe de caméra en perspective : déplacement de l'œil seul, de l'œil et du point visé,
  ou décalage de projection ? Rôle de `parallaxDepth` des modèles.
- **QO-3D-27 [Inféré]** — Ordre de composition des angles d'Euler 3D (`Rz·Ry·Rx` choisi) et sens de rotation. Les
  aperçus ne permettent pas de trancher : celui de `dna_fragment` n'a pas été pris depuis la caméra de la scène.
- **QO-3D-28 [Inféré]** — Modèle enfant d'une image, ou image enfant d'un modèle, dans une scène 3D : composition des
  repères 2D et 3D.
- **QO-3D-29 [Observé]** — Alpha écrit par les modèles dans `_rt_FullFrameBuffer` : visible par un effet `copybackground`
  qui relit l'alpha ? Masque d'écriture (`alphawriting`).
- **QO-3D-30 [Inféré]** — `transparentsorting` : granularité (objet ou maillage), critère de distance, place
  relative des images et des particules.
- **QO-3D-31 [Observé / Inféré]** — `_rt_Reflection` : plan fixe `y = 0` ou plan de l'objet qui lit la réflexion ?
  Couleur d'effacement ? Taille ? Sens de lecture vertical exact dans les shaders sans branche `HLSL` (`grid`,
  `generic`) : WE dessine-t-il la RT déjà retournée ? À valider par F-3D-R2.
- **QO-3D-32 [Inféré]** — Image non `fullscreen` dans une scène perspective : plan monde à `origin` (pixels ou unités
  monde ?), toujours face à la caméra ?
- **QO-3D-33 [Inféré]** — Particules en perspective : quelles inversions de Y de la spec 05 §2.2 gardent un sens ?
  Taille des sprites en unités monde.

---

## Amendements proposés aux autres specs

Proposés à leurs rédacteurs ; ce document ne les applique pas.

- **Spec 01 §3.1** : renvoyer au §1.10 pour les `.mdl` d'objets modèle (`MDLV0004`, `0014`, `0017`, `0023`
  statiques). L'heuristique ne reste qu'aux marionnettes non structurées.
- **Spec 02** :
  - §2 : `type` absent → scène (QO-3D-1) ;
  - §6.2 et §6.3 : lire `fov`, `nearz`, `farz` dans `general` (Q5) ; défauts 50 / 0,01 / 10000 ; `orthogonalprojection`
    absente ou `null` → scène perspective (Q6) ; clés `clearenabled`, `transparentsorting`, `lightconfig`, `hdr` ;
  - §7.1 : classification §1.4 (`model`, `light`, `sprite`, valeurs `null`) ;
  - §7.2 : identifiants synthétiques pour les objets sans `id` (§1.8) ;
  - §8 : compagnon `models/<nom>.json` d'un `.mdl` (§1.9) ;
  - §9 : `usershadervalues` (§1.11.3), défauts d'état des matériaux de modèle (§1.11.2), alias `depthwriting` et
    `depthtesting` ;
  - Q21 : défauts du bloom 2,0 / 0,65 ;
  - Q31 : échec de modèle non fatal.
- **Spec 03** :
  - §3.3 : `CASTU`, `CASTF`, `texSample2DCompare`, `DECLARE_SAMPLER2D_PARAMETER`, `MAKE_SAMPLER2D_ARGUMENT`,
    `texLoad2D`, `clip` ; macro `HLSL` pour les passes 3D (§0.3) ;
  - §3.4 : attributs `a_Normal`, `a_Tangent4`, `a_TexCoordVec4` ;
  - §4.2 : contenu réel du module `LightingV1` (§2.4) ;
  - §4.3 : marqueurs `// [PASS] ` et `// [COMBO_DISABLED] ` ;
  - §6.2 : clé `formatcombo` ;
  - §12.2 : uniforms du §2.9 ; `g_LightAmbientColor` et `g_LightSkylightColor` vifs ;
  - §12.4 : téléverser tous les éléments d'un tableau de vecteurs (`g_LightsPosition[4]`, `g_LightsColorRadius[4]`) ;
  - Q10 : `sampler2DComparison` = sampler de comparaison.
- **Spec 04** :
  - §0.2 : seconde convention d'espace clip pour les passes 3D (§0.3) ;
  - §3 et §4.1 : taille de scène perspective = taille de sortie ;
  - §4.3 : RT `_rt_Reflection` et mips de `_rt_MipMappedFrameBuffer` ;
  - §5 et §11.2 : tampon de profondeur quand la scène a des modèles ;
  - §6 : caméra perspective, chemins, fondu, tremblement ;
  - §20.1 : renvoi à cette spec ;
  - Q4, Q23.
- **Spec 05** : particules d'une scène perspective (§3.9) : espace monde, VP de la caméra, `g_EyePosition` de la
  caméra, pas d'œil fixe en (0, 0, 1000).
- **Spec 06** : `IScene.fov`, `nearz`, `farz`, `ambientcolor`, `skylightcolor`, `camerafade`, `camerashake*`
  (écriture vivante) ; `getCameraTransforms` et `setCameraTransforms` ; `ILayer.angles` en degrés pour les objets 3D ;
  `IModelLayer.perspective` ; `createModelData` (format de sommet `POSITION`, `NORMAL`, `UV`, `TANGENT_SIGNED`,
  `COLOR` ; indices `u16` ; ordre trigonométrique), à spécifier à part.
- **Spec 08** : §2.1, le champ « réservé » à l'offset 13 est le nombre de skins `S` ; §2.3, bit `0x20`.
