# Spec 08 — Marionnettes « puppet warp » : squelette, skinning, animations, attachements

Statut : rédigée en clean-room le 2026-11-03. Aucune ligne de code amont n'est reproduite.
Public : implémenteurs Rust de `lumen-formats` (lecture `.mdl`), `lumen-scene` (animation, attachements),
`lumen-render` et `lumen-shader` (skinning GPU), `lumen-script` (API SceneScript des os et calques d'animation).

Mots normatifs : **DOIT**, **NE DOIT PAS**, **DEVRAIT**, **PEUT**.

## Portée et motif

L'amont lit seulement le maillage d'un fichier de marionnette (heuristique de la spec 01 §3.1) et le dessine en
**pose de repos**. Il ignore squelette, poids, animations et attachements. Wallpaper Engine, lui, déforme le maillage
selon des os animés, et les calques enfants suivent ces os. Exemple : sur le fond 3463520581, la chevelure d'Asuna
et le corps entier suivent les os animés. Application de `docs/PARITY-POLICY.md`, règle 4 : Lumen DOIT implémenter
ce comportement documenté par WE. C'est un sur-ensemble de l'amont, pas une déviation.

Couvert : format binaire complet des sections `MDLV`, `MDLS`, `MDAT`, `MDLA` et `MDLE` tel qu'observé ; calcul des
matrices d'os par frame ; skinning ; calques d'animation ; attachement de calques enfants ; uniforms, combos et
attributs shader ; fixtures et vecteurs de test.

Hors périmètre : modèles 3D statiques (`MDLV0004`, `0014`, `0017`, et `0023` sans squelette), éclairage. Les
fonctions WE documentées mais absentes du corpus (physique des os, cinématique inverse, « blend rules », blend
shapes) sont listées en §11 sans format connu.

## Sources et étiquettes

| Étiquette | Signification |
|---|---|
| **[Doc WE]** | Documentation publique Wallpaper Engine (docs.wallpaperengine.io : *Puppet Warp Introduction*, *Attachment Points*, *Blend Rules*, *Bone Physics Simulation*, *Inverse Kinematics*, *Interactive Puppet Warps*, *Extending an existing Puppet Warp*, *Timeline: Single & Mirrored*, références SceneScript `IImageLayer`, `IAnimationLayer`, `IAnimation`). |
| **[Observé]** | Analyse octet par octet des fichiers réels disponibles en lecture : les 8 `.mdl` du `scene.pkg` (`PKGV0023`) du fond 3463520581, les 28 `.mdl` statiques du dossier d'installation WE (`assets/` et `projects/defaultprojects/`), le `scene.json` de 3463520581 et les shaders `assets/shaders/*`. |
| **[Inféré]** | Déduction cohérente avec les données, non confirmée. Chaque cas renvoie à une question ouverte. |
| **[Amont]** | Comportement de l'implémentation de référence (pour mémoire : maillage seul, pose de repos). |

Aucun fichier WE n'est copié dans le dépôt. Les tests sur données réelles sont `#[ignore]` (voir `docs/TESTING.md`).

---

## 0. Conventions

### 0.1 Types

Types de la spec 01 §0.1 : `u8`, `u16`, `u32`, `i32`, `f32` (little-endian), `cstr` (octets jusqu'au premier
`0x00`, consommé), `magic9` (8 caractères ASCII puis `0x00`). En plus :

| Notation | Taille | Description |
|---|---|---|
| `mat4` | 64 | 16 × `f32`, **colonne-majeure** : l'élément `m[4c + r]` est en ligne `r`, colonne `c`. La translation occupe `m[12]`, `m[13]`, `m[14]`. Convention vecteur-colonne (`M · v`). [Observé : translations toujours en 12–14, `m[15]` = 1.] |
| `vec3` | 12 | 3 × `f32`. |

Tous les décalages « absolus » ci-dessous sont comptés depuis le **début du fichier `.mdl`**, et non depuis le
début du `.pkg` qui le contient.

### 0.2 Espace maillage (EM)

Positions des sommets, transformations d'os et attachements sont exprimés dans un même repère, l'**espace
maillage** :

- origine au **centre** de l'image du calque ;
- x vers la droite, y vers le **haut** ;
- unité : 1 pixel de l'image à l'échelle 1 (même unité que `size` du calque).

[Observé] Pour tout fichier sans bloc de mise en page (§2.6.1), chaque sommet vérifie exactement
`x = u·W − W/2` et `y = H/2 − v·H`, où (W, H) est la `size` du calque dans `scene.json` et (u, v) les coordonnées
de texture du sommet (écart max 0,0 sur 4 fichiers). La conversion de l'amont vers les pixels locaux
`(S.x/2 + x, S.y/2 − y)` (spec 01 §3.1.2 étape 7) est cohérente avec ce repère.

### 0.3 Angles

Angles d'os en **radians** dans les fichiers. Rotation `Rz(a)` standard dans l'EM (y vers le haut), donc
sens trigonométrique à l'écran pour `a > 0`, comme `angles.z` des objets (spec 04 §8.4). [Inféré, QO-P10]

---

## 1. Vue d'ensemble d'un fichier `.mdl`

```
+---------------------------+  décalage 0
| MDLV (pas de fin chaînée) |  en-tête + n blocs de maillage
+---------------------------+  décalage F1 = fin du dernier bloc de maillage
| section 1 : magic9, u32 fin, contenu  |  fin = décalage absolu de la section suivante
| section 2 : magic9, u32 fin, contenu  |
| ...                                   |
+---------------------------+
| 0x00                      |  octet terminal (« magic vide »)
+---------------------------+  fin de fichier
```

Sections observées, toujours dans cet ordre (chaque section facultative peut manquer) :

| Section | Version observée | Présence (8 fichiers) | Rôle | Nécessaire au rendu |
|---|---|---|---|---|
| `MDLV` | `0023` | 8/8 | Maillage : sommets avec indices et poids d'os, indices de triangles. | oui |
| `MDLS` | `0004` | 8/8 | Squelette : os, hiérarchie, pose de repos. Données d'éditeur en fin. | oui (os) |
| `MDAT` | `0001` | 6/8 | Points d'attachement : nom, os porteur, transformation. | oui (enfants) |
| `MDLA` | `0006` | 5/8 | Animations : une piste par os, un échantillon par image. | oui (animation) |
| `MDLE` | `0002` | 3/8 | Pose « planche » (mise en page de la texture), par os. | non |

[Observé] Le `u32` qui suit chaque magic9 de section (sauf `MDLV`) vaut le décalage absolu de la fin de la
section. Pour toutes les sections sauf la dernière, ce décalage est celui du magic suivant. Pour la dernière, il
vaut `taille_fichier − 1` et pointe sur l'octet `0x00` final.

---

## 2. Bloc `MDLV` (version `0023`)

### 2.1 En-tête

| Décalage | Type | Champ | Valeurs observées |
|---|---|---|---|
| 0 | `magic9` | `MDLV0023` + `0x00` | `MDLV0023` (puppets et quelques modèles statiques), `MDLV0004`, `MDLV0014`, `MDLV0017` (modèles statiques, hors périmètre). |
| 9 | `u32` | drapeaux du modèle | `0x01800009` sur toutes les marionnettes. `0x09`, `0x0B`, `0x0F`, `0x27` sur les modèles statiques. Non interprété (QO-P15). |
| 13 | `u32` | réservé | 1 (2 sur un modèle statique `MDLV0004`). Non interprété. |
| 17 | `u32` | `nm` = nombre de blocs de maillage | 1 sur toutes les marionnettes ; 3, 4, 6 sur des modèles statiques. |
| 21 | — | `nm` blocs de maillage (§2.2) | |

### 2.2 Bloc de maillage

| Ordre | Type | Champ | Notes |
|---|---|---|---|
| 1 | `cstr` | chemin du matériau | ex. `materials/<nom>.json`. Informatif : le matériau effectif est celui du JSON du modèle. |
| 2 | `u32` | réservé | 0 partout. |
| 3 | 6 × `f32` | boîte englobante (min xyz, max xyz) | Nulle sur toutes les marionnettes ; renseignée sur les modèles statiques. Non utilisée. |
| 4 | `u32` | `vf` = format de sommet | §2.3. `0x0180000F` sur toutes les marionnettes. |
| 5 | `u32` | `vb` = taille des sommets en octets | multiple de la taille de sommet. |
| 6 | `vb` octets | sommets | §2.4. |
| 7 | `u32` | `ib` = taille des indices en octets | multiple de 6 (triangles). |
| 8 | `ib` octets | indices | `ib/2` × `u16`, liste de triangles. |
| 9 | — | queue du bloc | §2.6. |

### 2.3 Format de sommet (`vf`)

[Observé] Chaque bit présent ajoute un champ. Les champs se suivent dans l'ordre de la table, sans remplissage :

| Bit | Masque | Champ | Type | Attribut shader correspondant |
|---|---|---|---|---|
| 0 | `0x00000001` | position | 3 × `f32` | `a_Position` |
| 1 | `0x00000002` | normale | 3 × `f32` | `a_Normal` |
| 2 | `0x00000004` | tangente (w = signe de la bitangente) | 4 × `f32` | `a_Tangent4` |
| 23 | `0x00800000` | indices d'os | 4 × `u32` | `a_BlendIndices` (uvec4) |
| 24 | `0x01000000` | poids d'os | 4 × `f32` | `a_BlendWeights` |
| 3 | `0x00000008` | coordonnées de texture | 2 × `f32` | `a_TexCoord` |

Vérifié : `0x0180000F` donne 80 octets, `0x09` donne 20 octets, `0x0F` donne 48 octets. Tout autre bit
(`0x20` vu sur un modèle statique) a une taille inconnue. Lumen DOIT alors abandonner la lecture structurée
(§2.8).

### 2.4 Sommet de marionnette (80 octets)

| Décalage dans le sommet | Type | Champ | Observé |
|---|---|---|---|
| +0 | 3 × `f32` | position (EM), z = 0 | |
| +12 | 3 × `f32` | normale | (0, 0, 1) partout |
| +24 | 4 × `f32` | tangente | ≈ (1, 0, 0, 1) |
| +40 | 4 × `u32` | indices d'os j0…j3 | indices d'os de `MDLS` |
| +56 | 4 × `f32` | poids w0…w3 | voir ci-dessous |
| +72 | 2 × `f32` | (u, v) | v = 0 en haut de la texture |

[Observé] sur les poids, sur les 8 fichiers :

- au plus 4 influences par sommet (4 atteintes dans un fichier, 2 dans un autre, 1 ailleurs) ;
- somme des poids égale à 1 à 1e-7 près ;
- les poids ne sont **pas** triés, et un poids nul peut précéder un poids non nul (4 sommets) ;
- un emplacement de poids nul porte l'indice 0 ;
- un os sans parent (racine) porte un poids non nul seulement quand c'est l'unique os du fichier.

### 2.5 Indices

Liste de triangles, `u16`. Un indice `≥ nv` (nombre de sommets) invalide le maillage, comme la spec 01 §3.1.2
étape 5. [Observé] Les triangles sont déjà rangés dans l'**ordre de dessin** des parties (§2.6.2, §4.4). Dessiner le
tampon dans l'ordre donne la superposition voulue, sans test de profondeur.

### 2.6 Queue du bloc de maillage

Elle suit immédiatement les indices :

| Ordre | Type | Champ |
|---|---|---|
| 1 | `u8` | `aMiseEnPage` : 0 ou 1 |
| 2 | si `aMiseEnPage` = 1 : `u32` réservé (observé 1), `u32` `lb` (= 12 × nv), `nv` × `vec3` | positions « planche » (§2.6.1) |
| 3 | `u8` | `aPlages` : 0 ou 1 |
| 4 | si `aPlages` = 1 : `u32` `lp` (multiple de 16), puis `lp/16` entrées de 4 × `u32` | plages d'indices par partie (§2.6.2) |
| 5 | `u32` | réservé, observé 0 |

[Observé] Marionnettes : `aPlages` = 1 partout, `aMiseEnPage` = 1 dans 3 fichiers sur 8. Modèles statiques
`MDLV0023` : `00 00 00 00 00 00` (deux drapeaux nuls puis `u32` 0). Un bloc de maillage suivant, s'il existe,
commence juste après.

#### 2.6.1 Positions « planche » (mise en page de la texture)

Une position par sommet, dans l'EM, telle que les parties sont disposées **dans la texture** (planche de
personnage, [Doc WE] *Extending*). [Observé] Ces positions valent exactement `(u·W − W/2, H/2 − v·H, 0)` (écart
0,0 sur les 3 fichiers). Elles diffèrent des positions de rendu (§2.4) parce que l'auteur a déplacé des parties
avec leurs os. Sur un sommet rigide (un seul os b) : `planche − position = Wb_planche − Wb_repos`, où
`Wb_planche` vient de `MDLE` (§7) et `Wb_repos` de `MDLS`. **Lumen NE DOIT PAS les utiliser pour le rendu.** Il
PEUT les ignorer.

#### 2.6.2 Plages par partie

Chaque entrée vaut (`os`, `réservé` = 0, `premier_indice`, `nombre_d_indices`), en indices et non en octets. Une
entrée par os, dans l'ordre de dessin. La plage de l'os racine est vide. [Observé] Les entrées couvrent le tampon
d'indices de manière contiguë. Les triangles d'une plage ont leur poids dominant sur cet os. Sur
« asuna body bottom », l'ordre est 0, 6, 5, 1, 3, 4, 2, identique à l'ordre de dessin de `MDLS` (§4.4). Information
redondante pour le rendu : Lumen PEUT l'ignorer.

### 2.7 Fin du bloc `MDLV`

`MDLV` n'a pas de décalage de fin. La première section chaînée commence juste après le dernier bloc de maillage
(décalage F1).

### 2.8 Lecture structurée et repli

1. Si la version vaut `MDLV0023`, que `nm` = 1, que `vf` = `0x0180000F` et que toutes les lectures restent dans le
   fichier : lecture structurée selon §2.1–2.6, F1 = fin de la queue.
2. Sinon, et pour `MDLV0021` (acceptée par l'amont, absente du corpus, QO-P16) : heuristique de la spec 01 §3.1.2,
   sommets de 80 octets ; les indices et poids d'os se lisent aux décalages +40 et +56 de chaque sommet ;
   F1 = première occurrence de `MDLS` (règle `L` de la spec 01).
3. [Observé] Sur les 8 fichiers, l'heuristique trouve le bloc exactement au champ `vf` de la lecture structurée :
   les deux voies donnent le même maillage.

---

## 3. Chaînage des sections

À partir de `p = F1` :

1. Si `p ≥ taille` ou `D[p] = 0x00` : fin normale.
2. Lire `magic9` en `p`. Le 9ᵉ octet DOIT valoir `0x00`, sinon fin, avec un avertissement si aucune section n'a été
   lue.
3. Lire `fin` = `u32` en `p + 9`. Si `fin ≤ p + 13` ou `fin > taille` : section invalide, avertissement, fin du
   chaînage (les sections déjà lues restent valides).
4. Selon les 4 premiers caractères et la version (4 chiffres) : lire le contenu borné à `[p + 13, fin)`. Toute
   lecture hors de cet intervalle est une erreur de la section (§8). Section inconnue : ignorée sans erreur.
5. `p = fin` ; retour en 1.

Une section dont le contenu lu s'arrête avant `fin` est acceptée : les octets restants sont ignorés. Seul
le début des sections est nécessaire au rendu.

---

## 4. Section `MDLS0004` — squelette

### 4.1 Disposition

| Ordre | Type | Champ |
|---|---|---|
| 1 | `magic9` | `MDLS0004` |
| 2 | `u32` | fin de section |
| 3 | `u32` | `n` = nombre d'os (observé 1 à 15) |
| 4 | `n` × enregistrement d'os | §4.2 |
| 5 | 11 octets | bloc réservé, observé entièrement nul (QO-P1) |
| 6 | `u8` + `n` × 76 octets | boîtes d'os (§4.3), `u8` observé 1 |
| 7 | `u8` + `n` × `u32` | ordre de dessin (§4.4), `u8` observé 1 |
| 8 | `u8` + `n` × `u32` | clés de tri (§4.4), `u8` observé 1 |

Lumen DOIT lire les points 1 à 4 et PEUT ignorer le reste (saut à la fin de section).

### 4.2 Enregistrement d'os

| Ordre | Type | Champ | Observé |
|---|---|---|---|
| 1 | `cstr` | nom | vide en général ; quelques noms (`legs`, `skirt`). |
| 2 | `u32` | type | 0 pour l'os d'indice 0 des squelettes à plusieurs os (racine sans sommet), 1 sinon (y compris l'os unique d'un squelette à un os). Sans effet sur le rendu. [Doc WE : « root bones » = points de référence] |
| 3 | `i32` | `parent` | −1 pour une racine ; sinon indice d'un os **antérieur** (toujours `< index`). |
| 4 | `u32` | `tm` = taille de la transformation | 64 partout. |
| 5 | `tm` octets | transformation de repos **locale** (relative au parent) : `mat4` si `tm` = 64 | translation seule (rotation identité, échelle 1) dans tout le corpus. |
| 6 | `cstr` | données supplémentaires | toujours vide (`0x00`). [Inféré : chaîne JSON de contraintes ou de physique, QO-P2] |

[Observé] Les transformations sont **locales**. En les composant le long de la hiérarchie, on obtient des chaînes
d'os régulières : épaule, coude, poignet sur « kirito arm », par segments de 210 à 283 px. Les sommets pondérés
sur un os se trouvent près de sa position monde (ex. sommet (21,5 ; 412,0) pour un os en (24,1 ; 415,2)). Lues
comme absolues, elles n'auraient pas de sens.

Règles de robustesse (choix Lumen) : un `parent` ≥ `index` ou ≥ `n` est traité comme −1 (avertissement) ;
si `tm` ≠ 64, la section est rejetée (§8).

### 4.3 Boîtes d'os (données d'éditeur)

Par os : `vec3` demi-dimensions puis `mat4`, dans le repère de l'os. [Observé] Pour un os sans sommet pondéré :
demi-dimensions (0,5 ; 0,5 ; 0,5) et translation (0,5 ; 0,5 ; 0,5). Sinon, une boîte orientée qui englobe ses
sommets, avec z = demi-dimension z = min(x, y). [Inféré : sélection dans l'éditeur.] Non utilisées.

### 4.4 Ordre de dessin

- Tableau 1 : les indices d'os triés dans l'ordre de dessin.
- Tableau 2 : par os, une clé entière (multiples de 100 observés) ; le tableau 1 en est le tri croissant.

[Observé] « asuna body bottom » : clés (0, 700, 1000, 800, 900, 600, 500), ordre (0, 6, 5, 1, 3, 4, 2), identique
aux plages du §2.6.2. Les autres fichiers ont l'ordre identité. L'ordre est **déjà appliqué** au tampon d'indices :
Lumen n'a rien à faire.

---

## 5. Section `MDAT0001` — points d'attachement

| Ordre | Type | Champ |
|---|---|---|
| 1 | `magic9` | `MDAT0001` |
| 2 | `u32` | fin de section |
| 3 | `u16` | `na` = nombre d'attachements (observé 1 ou 2) |
| 4 | `na` × : `u16` os, `cstr` nom, `mat4` transformation | |

- `os` : indice dans `MDLS`. Une valeur `≥ n` rend l'attachement invalide : il est ignoré, avec avertissement.
- `nom` : comparé **octet à octet, sensible à la casse**, à la clé `attachment` des objets enfants (§9.10). En cas
  de doublon, le premier l'emporte.
- transformation : **relative à l'os** porteur (repère local de l'os). [Inféré, QO-P11] Observé : translation
  seule. Exemple : sur « asuna body », l'attachement `head` est porté par l'os 2, en (−32,5 ; 116,4) dans son
  repère, ce qui donne (45,0 ; 369,0) dans l'EM, au niveau de la tête.

---

## 6. Section `MDLA0006` — animations

### 6.1 Disposition

| Ordre | Type | Champ |
|---|---|---|
| 1 | `magic9` | `MDLA0006` |
| 2 | `u32` | fin de section |
| 3 | `u32` | `nA` = nombre d'animations (observé 1 ou 2) |
| 4 | `nA` × animation | §6.2 |

### 6.2 Animation

| Ordre | Type | Champ | Observé |
|---|---|---|---|
| 1 | `u32` | `id` | identifiant référencé par `animationlayers[].animation` du `scene.json` (vérifié sur les 7 couples). |
| 2 | `u32` | réservé | 0 |
| 3 | `cstr` | nom | `Animation 1`, `eyes`… (sert à la recherche par nom en SceneScript). |
| 4 | `cstr` | mode | `loop` partout. [Doc WE] modes *Loop*, *Mirror*, *Single* ; chaînes `mirror` et `single` [Inféré, QO-P7]. |
| 5 | `f32` | `fps` | 7,5 ; 10 ; 12 ; 15 ; 20 |
| 6 | `u32` | `N` = nombre d'images | 60 partout |
| 7 | `u32` | réservé | 0 |
| 8 | `u32` | `nt` = nombre de pistes | = nombre d'os partout |
| 9 | `nt` × piste | §6.3 | |
| 10 | 35 octets | bloc réservé de fin d'animation | entièrement nul partout (QO-P3) |

Durée : `D = N / fps` secondes.

### 6.3 Piste

| Ordre | Type | Champ |
|---|---|---|
| 1 | `u32` | réservé, observé 0. Ce n'est **pas** un indice d'os : il vaut 0 sur toutes les pistes. |
| 2 | `u32` | `lk` = taille des échantillons en octets (observé 2196 = 61 × 36) |
| 3 | `lk/36` × échantillon | §6.4 |

La piste d'indice `k` s'applique à l'os `k`, dans l'ordre de `MDLS`. [Observé] Il y a `N + 1` échantillons, un par
image de 0 à N incluse : les courbes de l'éditeur sont **précalculées image par image** (courbes d'accélération
visibles dans les valeurs). Pour un os non animé, toutes les valeurs égalent sa pose de repos locale.

### 6.4 Échantillon (36 octets)

| Décalage | Type | Champ |
|---|---|---|
| +0 | 3 × `f32` | position locale (même sens que la translation de repos) |
| +12 | 3 × `f32` | angles locaux (x, y, z) en radians ; seul z varie dans le corpus |
| +24 | 3 × `f32` | échelle locale |

Valeurs **absolues** (et non des écarts au repos). [Observé] L'image 0 d'une piste non animée vaut exactement la
translation de repos. Les clignements des yeux (animation `eyes`) font passer l'échelle y de 1 à 0,013.

---

## 7. Section `MDLE0002` — pose « planche » (données d'éditeur)

| Ordre | Type | Champ |
|---|---|---|
| 1 | `magic9` | `MDLE0002` |
| 2 | `u32` | fin de section |
| 3 | `u32` | `le` = 64 × n |
| 4 | `n` × `mat4` | transformation locale de chaque os dans la disposition de la texture |

[Observé] Présente exactement dans les 3 fichiers qui ont des positions « planche » (§2.6.1). Pour chaque os b et
chaque sommet rigide sur b : `planche − position = Monde_MDLE(b) − Monde_MDLS(b)` (écart 0,00 sur toutes les
parties). Ce n'est pas une pose de rendu : Lumen NE DOIT PAS l'utiliser pour le skinning et PEUT ignorer la
section.

---

## 8. Versions et erreurs

| Section | Versions acceptées | Autre version | Erreur de contenu (lecture hors section, `tm` ≠ 64, indice invalide…) |
|---|---|---|---|
| `MDLV` | `0023` structurée ; `0021` par heuristique (spec 01) | échec non fatal de la spec 01 : quadrilatère standard | spec 01 |
| `MDLS` | `0004` | section ignorée → **pas de squelette** | pas de squelette |
| `MDAT` | `0001` | ignorée → aucun attachement | aucun attachement |
| `MDLA` | `0006` | ignorée → aucune animation | les animations entièrement lues avant l'erreur sont gardées |
| `MDLE` | `0002` | ignorée | ignorée |

« Pas de squelette » : maillage en pose de repos (comportement actuel de Lumen), attachements et animations
inactifs. Toutes ces erreurs sont **non fatales** et journalisées avec chemin, section et décalage (règle 2 de
la politique de parité).

Validation du skinning au chargement : si un sommet a un poids non nul sur un indice `≥ n`, le squelette est
rejeté (pose de repos, avertissement). Ce choix de Lumen évite un accès hors tableau dans le shader.

Bloc de 35 octets de fin d'animation non nul : l'animation courante est gardée, les **suivantes** sont abandonnées
(leur position n'est plus sûre), avec avertissement.

---

## 9. Sémantique d'exécution

### 9.1 Données préparées au chargement

Pour chaque os b (dans l'ordre des indices, les parents d'abord) :

- `L0_b` = transformation de repos locale (`mat4` de `MDLS`) ;
- `W0_b = W0_parent · L0_b` (ou `L0_b` pour une racine) ;
- `I_b = (W0_b)⁻¹` (inverse de liaison) ;
- pose de repos décomposée `R_b = (pos, ang, scl)` : `pos` = colonne 3 ; `scl` = normes des colonnes 0, 1, 2 ;
  `ang` = angles d'Euler de la rotation normalisée, ordre de composition de §9.6. Dans tout le corpus :
  `ang` = 0 et `scl` = 1.

### 9.2 Calques d'animation (`animationlayers` du `scene.json`)

Clés d'un élément (complète la spec 02 §7.5) :

| Clé | Mode | Défaut | Observé (3463520581) | Rôle |
|---|---|---|---|---|
| `id` | R entier | — | | identifiant du calque |
| `name` | O chaîne | `""` | `Animation 1`, `eyes`, `breathing` | nom du calque (SceneScript `getAnimationLayer(nom)`) |
| `animation` | L entier | 0 | 264, 2385, 1953… | `id` d'une animation de `MDLA` ; aucune correspondance → calque inactif (avertissement) |
| `rate` | L | 1.0 | 1.0 | multiplicateur de vitesse |
| `blend` | L | 1.0 | 1.0 | poids du calque |
| `visible` | L | faux (spec 02) | vrai | calque appliqué ou non (QO-P18) |
| `additive` | O booléen | faux [Inféré] | vrai partout | mode de mélange (§9.5) |
| `blendin` | O booléen | faux | faux | rampe d'entrée |
| `blendout` | O booléen | faux | faux | rampe de sortie |
| `blendtime` | O nombre | 0,5 [Inféré] | 0,5 | durée des rampes, en secondes |

Ordre d'application : ordre du tableau. Un calque créé par script avec `autosort` vrai est rangé après les calques
non additifs et avant les additifs ([Doc WE] « after opaque layers and before additive layers »).

### 9.3 Horloge d'un calque

Chaque calque a un temps propre `τ` en secondes, mis à 0 à la création (chargement de la scène). Le calque joue
dès le départ.

- À chaque frame rendue hors pause : si le calque joue, `τ += dt · rate`. `dt` est le temps réel écoulé depuis la
  frame précédente (règle 6), **sans** la durée d'une pause (QO-P18). Lumen DEVRAIT accumuler `τ` en `f64` pour
  éviter la dérive.
- La position s'avance même si le `visible` **du calque** est faux. [Inféré] (Une **image** invisible, elle,
  n'avance pas ses calques : §9.11.)
- Image fractionnaire `f = τ · fps`. Si `N = 0` ou `fps ≤ 0`, on utilise l'échantillon 0.
- Position `φ ∈ [0, N]` selon le mode :
  - `loop` : `φ = f mod N`, modulo positif, dans [0, N).
  - `mirror` : `g = f mod 2N` ; `φ = g` si `g ≤ N`, sinon `φ = 2N − g`. [Doc WE] Le retour se joue « à la même
    vitesse » que l'aller.
  - `single` : `φ = clamp(f, 0, N)`. Quand `f` atteint N pour la première fois, le calque s'arrête sur
    l'image N ([Doc WE] « halts at the last frame ») et les rappels `addEndedCallback` sont appelés une fois. Un
    calque créé par `playSingleAnimation` est ensuite détruit ([Doc WE]).
  - mode inconnu : traité comme `loop`, avec avertissement.
- `rate` négatif : `τ` décroît ; mêmes formules (lecture à l'envers). [Inféré]

### 9.4 Échantillonnage d'une piste

`i = min(⌊φ⌋, N − 1)`, `α = φ − i`. Valeur = `lerp(éch[i], éch[i+1], α)`, composante par composante (position,
angles, échelle), en clampant les indices au dernier échantillon disponible si la piste est courte.
[Inféré, QO-P6 : interpolation linéaire entre images précalculées. Les fps faibles observés, 7,5 pour 60 images,
seraient saccadés sans interpolation.] Les angles sont interpolés sans repliement modulo 2π.

En boucle, l'échantillon N n'est atteint qu'à la limite. [Observé] Il diffère parfois légèrement de l'échantillon
0 (écart de 0,01 px), ce qui donne un petit saut au bouclage, comme dans WE.

### 9.5 Mélange des calques → pose locale

Pour chaque os b, partir de `P_b = R_b` (position, angles, échelle). Pour chaque calque visible, dans l'ordre, avec
`w = blend · rampe` (§9.5.1) > 0, et pour chaque os `b < nt` dont l'échantillon vaut `S_b` :

- **additif** : `P.pos += w·(S.pos − R.pos)` ; `P.ang += w·(S.ang − R.ang)` ; `P.scl += w·(S.scl − R.scl)`.
  [Inféré, QO-P8 : écart au repos, échelle additive]
- **non additif** : `P.x = P.x + w·(S.x − P.x)` pour chacune des trois composantes.

Un os `b ≥ nt` garde sa valeur. Remarque : avec un seul calque, les deux modes donnent le même résultat (le premier
calque part toujours du repos). Ils ne diffèrent qu'en empilant plusieurs calques. Sur 3463520581, « asuna body »
empile `eyes` et `breathing`, tous deux additifs, sur des os disjoints : les deux mouvements s'appliquent.

#### 9.5.1 Rampes

- `blendin` vrai : `r_in = clamp(t_jeu / blendtime, 0, 1)`, où `t_jeu` est le temps réel depuis le dernier
  `play()` ou la création.
- `blendout` vrai et mode `single` : `r_out = clamp((D − φ/fps) / blendtime, 0, 1)`.
- Sinon la rampe vaut 1. `blendtime ≤ 0` donne 1. `rampe = r_in · r_out`. [Inféré, QO-P9]

### 9.6 Surcharges par script, puis matrices monde

1. [Doc WE] Les animations s'appliquent d'abord ; les scripts ensuite peuvent les remplacer
   (« SceneScript can override all puppet warp animations reliably »). Une valeur écrite par
   `setLocalBone*` / `setBoneTransform` pendant l'`update` d'un script remplace la pose de l'os pour la frame en
   cours (détails : spec 06).
2. Transformation locale : `L_b = T(P.pos) · Rz(P.ang.z) · Ry(P.ang.y) · Rx(P.ang.x) · Sc(P.scl)`. [Inféré :
   ordre sans effet en 2D, seul z varie.]
3. `W_b = W_parent · L_b` (racine : `W_b = L_b`), parents d'abord.
4. Matrice de skinning : `K_b = W_b · I_b`. Au repos, `K_b` = identité.

Physique des os, cinématique inverse et « blend rules » ([Doc WE]) ne sont pas implémentées, faute de format
connu (QO-P4). Les os concernés suivent seulement l'animation.

### 9.7 Skinning des sommets

Pour un sommet de position `v` (EM), d'indices `j0…j3` et de poids `w0…w3` :

```
v' = (Σ_{i=0..3} w_i · K_{j_i}) · (v, 1)        (combinaison linéaire des matrices, puis application)
```

- **4 influences**, sans normalisation ni tri. C'est la formule des shaders WE (§9.8) : la parité de rendu impose
  de ne pas renormaliser (règle 3).
- Les influences de poids nul ne contribuent pas, quel que soit leur indice.
- Un sommet dont tous les poids sont nuls s'effondre sur l'origine de l'EM, comme dans le shader. Cas absent du
  corpus (QO-P17).
- Normale et tangente (non utilisées sans éclairage) : même combinaison sur la partie 3 × 3.

### 9.8 Intégration au rendu (amende la spec 04 §20)

Rappel de la spec 04 : la première passe d'une image à marionnette dessine le maillage indexé dans le composite
A (taille S), en pixels locaux `C(x, y, z) = (S.x/2 + x, S.y/2 − y, z)`, avec `M_copie`. Lumen DOIT dessiner les
positions **skinnées** `C(v')` à la place de `C(v)`.

**Voie GPU (recommandée, conforme à `docs/PERFORMANCE.md`).** Les shaders de matériau des marionnettes
(`genericimage4` observé dans `materials/puppet.json` ; aussi `genericimage2`, `genericimage3`,
`clippingmaskimage4`) contiennent déjà un chemin de skinning activé par des combos que le **moteur** fournit
(aucune annotation `[COMBO]` ne les déclare) :

| Élément | Nom | Valeur à fournir |
|---|---|---|
| combo | `SKINNING` | 1 si l'image a un squelette valide |
| combo | `BONECOUNT` | taille du tableau d'os : `max(1, n)` [Inféré, QO-P14] |
| uniform | `g_Bones[BONECOUNT]` | `mat4x3` par os (4 colonnes de 3 lignes) : les 3 premières lignes de `K'_b`, téléversées en colonne-majeure, soit 12 flottants par os `(K00, K10, K20, K01, K11, K21, K02, K12, K22, K03, K13, K23)` |
| attribut | `a_BlendIndices` | `uvec4` (attribut entier) ← indices +40 |
| attribut | `a_BlendWeights` | `vec4` ← poids +56 |
| attribut | `a_Normal` | `vec3` ← normale +12 |
| attribut | `a_Tangent4` | `vec4` ← tangente +24 (lu seulement si le combo `NORMALMAP` est actif) |
| attribut | `a_Position`, `a_TexCoord` | `C(v)` et (u, v), comme aujourd'hui |

Dans le dialecte WE, le produit d'un `vec4` par la somme pondérée des `g_Bones` se traduit en `M · v` (spec 03
§3.3). Comme les positions sont fournies en pixels locaux, Lumen DOIT téléverser la matrice conjuguée
`K'_b = C · K_b · C⁻¹`. Avec `(hx, hy) = S/2` et `K_b` de lignes `[a, b, 0, tx]`, `[c, d, 0, ty]`, `[0, 0, 1, tz]`
(cas 2D) :

```
K'_b = [  a  , −b , 0 , hx − a·hx + b·hy + tx ]
       [ −c  ,  d , 0 , hy + c·hx − d·hy − ty ]
       [  0  ,  0 , 1 , tz                    ]
```

En général : `K' = C · K · C⁻¹` en 4 × 4, avec `C = T(hx, hy, 0) · Sc(1, −1, 1)`. `g_Bones` n'est téléversé
que si une matrice a changé depuis la frame précédente. Aucune allocation par frame.

Autres uniforms des shaders liés aux marionnettes : `g_BonesAlpha[BONECOUNT]` (combo `SKINNING_ALPHA`) et les
blend shapes (`MORPHING`, `g_MorphOffsets[12]`, `g_MorphWeights[12]`, `g_Texture5`, `a_PositionVec4`,
`MORPHING_MODIFIERS`). Leurs données source ne figurent pas dans le corpus : Lumen NE DOIT PAS activer ces combos
(QO-P5, QO-P19).

**Voie CPU (repli).** Si l'unité vertex de la passe 0 ne référence pas `g_Bones` (shader sans chemin de skinning),
Lumen calcule `C(v')` sur CPU et réécrit un tampon de positions réutilisé, seulement quand la pose change. Le
comportement de WE dans ce cas n'est pas connu (QO-P13).

**Rognage.** Le composite A a la taille S : une partie déformée hors de [0, S.x] × [0, S.y] est coupée dès que
l'image passe par A, c'est-à-dire quand elle a des effets. [Doc WE] Le « Padding » de la géométrie existe pour
cela : les effets sont limités à la zone configurée.

**Passe unique redirigée vers la scène** (image sans effet, spec 04 Q22). Pour suivre WE (règle 5), le maillage
DEVRAIT être dessiné à la place du rectangle de l'image. Position RC d'un point en pixels locaux `(lx, ly)` :
`(X0 + lx·(X1 − X0)/S.x, Yh + ly·(Yb − Yh)/S.y)`, puis `M_ecran`. La décision finale appartient à la Q22 de la
spec 04.

**Ordre et états.** Tampon d'indices dans l'ordre du fichier (§2.5). Blending et états du matériau (spec 04
§20.3). Pas de test de profondeur.

### 9.9 Attachements : clé `attachment` des objets

Clé d'objet, complète la spec 02 §7.2 :

| Clé | Mode | Défaut | Notes |
|---|---|---|---|
| `attachment` | O chaîne | absent | Nom d'un attachement de la marionnette du **parent** (`parent`). Observé avec des objets image, enfants d'images à marionnette. [Doc WE] aussi pour les particules. |

Résolution, faite une fois au chargement puis à chaque changement de parent : le parent DOIT être une image dont
le modèle a une marionnette avec `MDAT`, et le nom DOIT exister dans cette section. Sinon la clé est ignorée
(avertissement) et l'objet est un enfant ordinaire. [Observé] Les noms se répètent entre fichiers (`hair back`
existe dans deux marionnettes) : la recherche se fait dans la marionnette du parent direct uniquement.

### 9.10 Transformation d'un enfant attaché (amende la spec 04 §8.1)

Repère de l'attachement à l'instant t, dans l'EM du parent : `A(t) = W_os(t) · M_att`, avec `W_os(t)` de §9.6
(pose finale, scripts compris) et `M_att` la `mat4` de `MDAT`.

Dans la chaîne de composition de la spec 04 §8.1, un enfant attaché reçoit un **nœud intermédiaire** entre son
parent et lui. On note `acc` la transformation résolue du parent (origine, échelle, angle), et l'on décompose
`A(t)` en :

- `a = (A[12], A[13], A[14])` (translation) ;
- `φ = atan2(A[1], A[0])` ;
- `σ = (|col0|, |col1|, |col2|)`, avec `σ.y` négatif si `det(partie 2 × 2) < 0`.

Ce nœud a pour origine `a`, pour angle `φ` et pour échelle `σ`. Il se compose sur `acc` par les mêmes formules
qu'un enfant (§8.1), puis l'enfant se compose sur ce nœud. L'`origin`, l'échelle et l'angle de l'enfant dans
`scene.json` sont donc **relatifs au repère de l'attachement**. [Doc WE] « jumps to the attachment point and will
follow it along all animations ». [Observé] Les origines de 3463520581 n'ont de sens que dans ce repère : la mèche
`HAIR BACK (BIG)` (945 × 864) est à (−266, −266) de l'attachement `hair back`, lui-même à ≈ +675 px du centre de
la marionnette. Elle se centre donc vers +410 px et couvre la tête. Relativement au centre de la marionnette, elle
tomberait au niveau des jambes.

L'EM du parent a son origine au point `origin` résolu du parent. L'alignement du parent est ignoré, comme toute
taille ou alignement du parent dans la spec 04 §8.1 (QO-P11). Opacité (`alpha`) et couleur ne sont pas héritées.
[Observé] Les marionnettes de contrôle de 3463520581 ont `alpha` = 0 et leurs enfants sont visibles.

Chaînage : un enfant attaché peut lui-même porter une marionnette avec ses attachements. Sur 3463520581 :
marionnette de contrôle → `asuna body bottom` (attachement `Attachment`) → `asuna body` (`Attachment bottom`) →
mèches (`head`). La résolution de la racine vers les feuilles (spec 04 §8.1) l'accepte naturellement, pourvu
que **toutes les poses** soient calculées **avant** la résolution des transformations (§9.11).

### 9.11 Ordre des opérations par frame

1. Avancer l'horloge des calques de toutes les images à marionnette visibles (§9.3).
2. Pour chaque marionnette, dans n'importe quel ordre : mélange (§9.5), `W_b`, `K_b` (§9.6).
3. Scripts (`update`, spec 06). Si un script modifie un os, recalculer `W`/`K` de cette marionnette.
4. Résolution des transformations de tous les objets (spec 04 §8.1, avec §9.10).
5. Rendu : téléversement de `g_Bones` si nécessaire, passes (spec 04).

Une image invisible n'avance pas ses calques. [Inféré] Une marionnette sans calque visible, sans script ni
attachement animé a une pose constante : la scène peut alors être considérée statique
(`docs/PERFORMANCE.md`).

### 9.12 SceneScript (pour la spec 06)

Correspondances avec le modèle ci-dessus ([Doc WE] `IImageLayer`, `IAnimationLayer`) :

| API | Modèle |
|---|---|
| `getBoneCount()` | `n` |
| `getBoneIndex(nom)` | premier os de ce nom ; −1 si absent [Inféré] (beaucoup d'os ont un nom vide, QO-P21) |
| `getBoneParentIndex(os)` | `parent` |
| `getBoneTransform` / `setBoneTransform` | `W_b` (« world », c'est-à-dire l'EM de la marionnette [Inféré]). L'écriture fixe `L_b = W_parent⁻¹ · W`. |
| `getLocalBoneTransform` / `set…` | `L_b` |
| `getLocalBoneAngles` / `set…`, `getLocalBoneOrigin` / `set…` | `P.ang` / `P.pos` (unité des angles en SceneScript : QO-P10) |
| `getAnimationLayer(nom ou index)`, `getAnimationLayerCount()`, `createAnimationLayer(nom ou config)`, `destroyAnimationLayer`, `playSingleAnimation` | liste ordonnée des calques (§9.2). Un nom désigne une animation `MDLA` pour la création, et le `name` du calque pour la recherche. |
| `IAnimationLayer.fps`, `frameCount`, `duration`, `name` | `fps`, `N`, `D`, nom du calque |
| `rate`, `blend`, `visible` | §9.2 (écriture vivante) |
| `play`, `pause`, `stop`, `isPlaying`, `getFrame`, `setFrame`, `addEndedCallback` | §9.3 : `stop` remet `τ` à 0 ; `getFrame` renvoie `φ` ; `setFrame(k)` fixe `τ = k / fps` |
| `applyBonePhysicsImpulse`, `resetBonePhysicsSimulation`, blend shapes | sans effet (physique et blend shapes non implémentées, QO-P4, QO-P5) |

---

## 10. Fixtures et vecteurs de test

### F-P1 — marionnette minimale : un os, un attachement, une animation à 2 échantillons (782 octets)

Fabriquée pour cette spec (aucune donnée WE). SHA-256 :
`6ae4dbdaa5d5108ea285c5a6ab8470ab5056caf190f2b188cf2e75de34d4d5bb`.

Structure (décalages hexadécimaux) :

| Décalage | Taille | Contenu |
|---|---|---|
| 0x000 | 9 | `MDLV0023\0` |
| 0x009 | 12 | drapeaux `0x01800009`, réservé 1, `nm` = 1 |
| 0x015 | 7 | `cstr` `m.json` |
| 0x01C | 4 | réservé 0 |
| 0x020 | 24 | boîte nulle |
| 0x038 | 4 | `vf` = `0x0180000F` |
| 0x03C | 4 | `vb` = 240 |
| 0x040 | 80 | s0 : pos (0, 0, 0), normale (0, 0, 1), tangente (1, 0, 0, 1), indices (0, 0, 0, 0), poids (1, 0, 0, 0), uv (0, 1) |
| 0x090 | 80 | s1 : pos (20, 0, 0), …, uv (1, 1) |
| 0x0E0 | 80 | s2 : pos (0, 20, 0), …, uv (0, 0) |
| 0x130 | 4 | `ib` = 6 |
| 0x134 | 6 | indices 0, 1, 2 |
| 0x13A | 2 | `aMiseEnPage` = 0, `aPlages` = 1 |
| 0x13C | 4 | `lp` = 16 |
| 0x140 | 16 | plage (os 0, 0, premier 0, nombre 3) |
| 0x150 | 4 | réservé 0 |
| 0x154 | 9 | `MDLS0004\0` |
| 0x15D | 4 | fin = 0x215 |
| 0x161 | 4 | `n` = 1 |
| 0x165 | 1 | nom vide |
| 0x166 | 4 | type 1 |
| 0x16A | 4 | parent −1 (`FF FF FF FF`) |
| 0x16E | 4 | `tm` = 64 |
| 0x172 | 64 | repos local : identité, translation (10, 0, 0) |
| 0x1B2 | 1 | `cstr` vide |
| 0x1B3 | 11 | bloc réservé nul |
| 0x1BE | 1 + 76 | drapeau 1, boîte : demi-dimensions (10, 10, 10), translation (0, 0, 10) |
| 0x20B | 5 | drapeau 1, ordre (0) |
| 0x210 | 5 | drapeau 1, clé (0) |
| 0x215 | 9 | `MDAT0001\0` |
| 0x21E | 4 | fin = 0x26A |
| 0x222 | 2 | `na` = 1 |
| 0x224 | 2 | os 0 |
| 0x226 | 4 | `cstr` `att` |
| 0x22A | 64 | identité, translation (0, 5, 0) |
| 0x26A | 9 | `MDLA0006\0` |
| 0x273 | 4 | fin = 0x30D |
| 0x277 | 4 | `nA` = 1 |
| 0x27B | 8 | `id` = 7, réservé 0 |
| 0x283 | 2 | nom `A` |
| 0x285 | 5 | mode `loop` |
| 0x28A | 4 | `fps` = 1,0 |
| 0x28E | 4 | `N` = 1 |
| 0x292 | 4 | réservé 0 |
| 0x296 | 4 | `nt` = 1 |
| 0x29A | 8 | piste : réservé 0, `lk` = 72 |
| 0x2A2 | 36 | éch. 0 : pos (10, 0, 0), angles (0, 0, 0), échelle (1, 1, 1) |
| 0x2C6 | 36 | éch. 1 : pos (10, 0, 0), angles (0, 0, π/2 = `DB 0F C9 3F`), échelle (1, 1, 1) |
| 0x2EA | 35 | bloc réservé nul |
| 0x30D | 1 | `00` terminal |

Hexadécimal complet :

```
0000: 4D 44 4C 56 30 30 32 33 00 09 00 80 01 01 00 00
0010: 00 01 00 00 00 6D 2E 6A 73 6F 6E 00 00 00 00 00
0020: 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
0030: 00 00 00 00 00 00 00 00 0F 00 80 01 F0 00 00 00
0040: 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
0050: 00 00 00 00 00 00 80 3F 00 00 80 3F 00 00 00 00
0060: 00 00 00 00 00 00 80 3F 00 00 00 00 00 00 00 00
0070: 00 00 00 00 00 00 00 00 00 00 80 3F 00 00 00 00
0080: 00 00 00 00 00 00 00 00 00 00 00 00 00 00 80 3F
0090: 00 00 A0 41 00 00 00 00 00 00 00 00 00 00 00 00
00A0: 00 00 00 00 00 00 80 3F 00 00 80 3F 00 00 00 00
00B0: 00 00 00 00 00 00 80 3F 00 00 00 00 00 00 00 00
00C0: 00 00 00 00 00 00 00 00 00 00 80 3F 00 00 00 00
00D0: 00 00 00 00 00 00 00 00 00 00 80 3F 00 00 80 3F
00E0: 00 00 00 00 00 00 A0 41 00 00 00 00 00 00 00 00
00F0: 00 00 00 00 00 00 80 3F 00 00 80 3F 00 00 00 00
0100: 00 00 00 00 00 00 80 3F 00 00 00 00 00 00 00 00
0110: 00 00 00 00 00 00 00 00 00 00 80 3F 00 00 00 00
0120: 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
0130: 06 00 00 00 00 00 01 00 02 00 00 01 10 00 00 00
0140: 00 00 00 00 00 00 00 00 00 00 00 00 03 00 00 00
0150: 00 00 00 00 4D 44 4C 53 30 30 30 34 00 15 02 00
0160: 00 01 00 00 00 00 01 00 00 00 FF FF FF FF 40 00
0170: 00 00 00 00 80 3F 00 00 00 00 00 00 00 00 00 00
0180: 00 00 00 00 00 00 00 00 80 3F 00 00 00 00 00 00
0190: 00 00 00 00 00 00 00 00 00 00 00 00 80 3F 00 00
01A0: 00 00 00 00 20 41 00 00 00 00 00 00 00 00 00 00
01B0: 80 3F 00 00 00 00 00 00 00 00 00 00 00 00 01 00
01C0: 00 20 41 00 00 20 41 00 00 20 41 00 00 80 3F 00
01D0: 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
01E0: 00 80 3F 00 00 00 00 00 00 00 00 00 00 00 00 00
01F0: 00 00 00 00 00 80 3F 00 00 00 00 00 00 00 00 00
0200: 00 00 00 00 00 20 41 00 00 80 3F 01 00 00 00 00
0210: 01 00 00 00 00 4D 44 41 54 30 30 30 31 00 6A 02
0220: 00 00 01 00 00 00 61 74 74 00 00 00 80 3F 00 00
0230: 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
0240: 80 3F 00 00 00 00 00 00 00 00 00 00 00 00 00 00
0250: 00 00 00 00 80 3F 00 00 00 00 00 00 00 00 00 00
0260: A0 40 00 00 00 00 00 00 80 3F 4D 44 4C 41 30 30
0270: 30 36 00 0D 03 00 00 01 00 00 00 07 00 00 00 00
0280: 00 00 00 41 00 6C 6F 6F 70 00 00 00 80 3F 01 00
0290: 00 00 00 00 00 00 01 00 00 00 00 00 00 00 48 00
02A0: 00 00 00 00 20 41 00 00 00 00 00 00 00 00 00 00
02B0: 00 00 00 00 00 00 00 00 00 00 00 00 80 3F 00 00
02C0: 80 3F 00 00 80 3F 00 00 20 41 00 00 00 00 00 00
02D0: 00 00 00 00 00 00 00 00 00 00 DB 0F C9 3F 00 00
02E0: 80 3F 00 00 80 3F 00 00 80 3F 00 00 00 00 00 00
02F0: 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
0300: 00 00 00 00 00 00 00 00 00 00 00 00 00 00
```

Résultats attendus de la lecture :

- `MDLV` structuré : 3 sommets, 3 indices, `vf` en 0x038, F1 = 0x154. L'heuristique de la spec 01 trouve aussi
  son bloc en p = 0x038 (même maillage).
- Chaîne de sections : `MDLS` (0x154 → 0x215), `MDAT` (0x215 → 0x26A), `MDLA` (0x26A → 0x30D), octet `00`
  en 0x30D : fin.
- 1 os : racine, repos local = T(10, 0, 0) ; `W0` = T(10, 0, 0) ; `I` = T(−10, 0, 0).
- 1 attachement `att` sur l'os 0, M_att = T(0, 5, 0).
- 1 animation : id 7, nom `A`, `loop`, fps 1, N = 1, D = 1 s, 1 piste de 2 échantillons.

### Vecteurs (tolérance 1e-4)

Notation : `θ` = angle z de l'os 0 après mélange ; `K = T(10,0,0)·Rz(θ)·T(−10,0,0)`. Sommets s0 (0, 0),
s1 (20, 0), s2 (0, 20).

| # | Situation | θ | K (lignes 1 et 2) | s0' | s1' | s2' | attachement A(t) : position, angle |
|---|---|---|---|---|---|---|---|
| V1 | aucun calque (repos) | 0 | identité | (0, 0) | (20, 0) | (0, 20) | (10, 5), 0 |
| V2 | calque `loop`, blend 1, t = 0,5 s | π/4 | [0.707107, −0.707107, 0, 2.928932] ; [0.707107, 0.707107, 0, −7.071068] | (2.9289, −7.0711) | (17.0711, 7.0711) | (−11.2132, 7.0711) | (6.4645, 3.5355), π/4 |
| V3 | `loop`, t = 1,0 s (bouclage) | 0 | identité | = V1 | | | (10, 5), 0 |
| V4 | `loop`, t = 0,25 s, `rate` = 2 | π/4 | = V2 | = V2 | | | = V2 |
| V5 | mode `mirror`, t = 1,5 s (`g` = 1,5 → `φ` = 0,5) | π/4 | = V2 | = V2 | | | = V2 |
| V6 | mode `single`, t = 3 s (arrêt sur l'image N, rappel de fin appelé une fois) | π/2 | [0, −1, 0, 10] ; [1, 0, 0, −10] | (10, −10) | (10, 10) | (−10, −10) | (5, 0), π/2 |
| V7 | additif, blend 0,5, t = 0,5 s | π/8 | [0.92388, −0.382683, 0, 0.761205] ; [0.382683, 0.92388, 0, −3.826834] | (0.7612, −3.8268) | (19.2388, 3.8268) | (−6.8925, 14.6508) | (8.0866, 4.6194), π/8 |
| V8 | deux calques additifs sur la même animation, blend 1 puis 0,5, t = 0,5 s | 3π/8 | | | | | |
| V9 | t = 0,5 s : calque non additif blend 1 (`rate` 1, donne π/4), puis calque non additif blend 0,5 (`rate` 2 : `f` = 1, bouclage, donne 0) | π/4 + 0,5·(0 − π/4) = π/8 | = V7 | = V7 | | | = V7 |
| V10 | `visible` faux, t = 0,5 s | 0 | identité | = V1 | | | = V1 |

V3 vérifie que l'échantillon N n'est pas tenu en mode `loop`. V5 et V6 utilisent F-P1 réécrit avec le mode
`mirror` ou `single` en 0x285. Ces chaînes font 7 octets avec le `00` (5 pour `loop`), donc tout ce qui suit 0x285
est décalé de +2 : la fin de `MDLA` (0x273) vaut 0x30F, l'octet terminal est en 0x30F et la taille passe à 784.

**Téléversement GPU** (V2, S = (40, 40), donc hx = hy = 20). `K'` lignes : [0.707107, 0.707107, 0, −5.355339] ;
[−0.707107, 0.707107, 0, 27.071068] ; [0, 0, 1, 0]. Les 12 flottants de `g_Bones[0]`, dans l'ordre :
`0.707107, −0.707107, 0, 0.707107, 0.707107, 0, 0, 0, 1, −5.355339, 27.071068, 0`. Contrôle : `C(s1)` = (40, 20)
devient (37.0711, 12.9289) = `C(s1')`.

**Mélange de deux os** (sans fichier). K0 = identité, K1 = T(0, 10, 0), sommet (5, 5) :

- poids (0,25 ; 0,75) sur (0, 1) : (5, 12,5) ;
- poids (0,25 ; 0,25), non normalisés : (2,5 ; 5,0). Pas de renormalisation.

**Enfant attaché** (V2, t = 0,5 s). Parent : origine (100, 200, 0), échelle (2, 2, 1), angle 0. Enfant
attaché à `att` : origine (3, 4, 0), échelle 1, angle 0.

- Nœud d'attachement : `a` = (6.464466, 3.535534), `φ` = π/4, `σ` = (1, 1, 1). Résolu : origine
  (112.928932, 207.071068), angle π/4, échelle (2, 2, 1).
- Enfant résolu : origine (111.514718, 216.970563), angle π/4, échelle (2, 2, 1).
- Au repos (V1) : nœud en (120, 210), angle 0 ; enfant en (126, 218).

### Variantes négatives de F-P1

| Modification | Attendu |
|---|---|
| 0x15D–0x160 (fin de `MDLS`) = `FF FF 00 00` | fin hors fichier : chaînage arrêté, pas de squelette ; maillage dessiné au repos ; avertissement |
| octet 0x271 : `36` → `37` (`MDLA0007`) | section ignorée ; squelette et attachement valides ; le calque qui vise l'id 7 est inactif |
| octets 0x16A–0x16D = `00 00 00 00` (os 0 parent de lui-même) | parent traité comme −1 ; avertissement ; résultat identique |
| octet 0x068 (s0, indice j0) = `01` | poids non nul sur l'indice 1 ≥ n : squelette rejeté, pose de repos |
| octet 0x2EA = `01` | animation 7 gardée (c'est la dernière) ; avertissement |
| `nt` (0x296) = 0 et suppression des 80 octets de piste 0x29A–0x2E9 ; fin de `MDLA` = 0x2BD | animation sans piste : l'os reste au repos |
| magic `MDLV0021` en 0x006–0x007 (`32 31`) | lecture par heuristique (spec 01) ; même maillage ; sections trouvées via la première occurrence de `MDLS` |

### F-P2 — données réelles (tests `#[ignore]`, `LUMEN_WE_WORKSHOP`)

Fichiers du `scene.pkg` de `3463520581` (archive `PKGV0023`, entrées `models/*_puppet.mdl`) :

| Fichier (`models/…_puppet.mdl`) | Taille | Sections | Sommets | Indices | Os | Attachements (nom : os) | Animations (id, nom, mode, fps, N) |
|---|---|---|---|---|---|---|---|
| `puppet` | 17572 | V S T A | 163 | 882 | 1 | `Attachment` : 0, `hair back` : 0 | (1953, `Animation 1`, loop, 10, 60) |
| `puppet - Copy` | 17503 | V S T A | 163 | 882 | 1 | `Attachment` : 0 | (1949, `Animation 1`, loop, 12, 60) |
| `asuna body bottom` | 91193 | V S T E | 872 | 4503 | 7 | `Attachment bottom` : 2 | — |
| `asuna body` | 171734 | V S T A E | 985 | 5481 | 15 | `head` : 2 | (264, `eyes`, loop, 20, 60), (2385, `Animation 2`, loop, 15, 60) |
| `hair back big chunk` | 21139 | V S T | 227 | 1197 | 2 | `hair back` : 1 | — |
| `main hair back c2` | 24775 | V S T | 266 | 1455 | 2 | `hair back 2` : 1 | — |
| `kirito arm` | 38410 | V S A | 317 | 1650 | 4 | — | (3330, `Animation 1`, loop, 7.5, 60) |
| `kirito face` | 67256 | V S A E | 487 | 2541 | 7 | — | (374, `Animation 1`, loop, 15, 60) |

(V = `MDLV0023`, S = `MDLS0004`, T = `MDAT0001`, A = `MDLA0006`, E = `MDLE0002`.)

Contrôles :

- toutes les pistes ont 61 échantillons ; nombre de pistes = nombre d'os ;
- pour chaque os non animé, échantillon 0 = translation de repos ;
- positions = `(u·W − W/2, H/2 − v·H)` à 1e-3 près avec W × H = `size` du calque, pour `hair back big chunk`
  (945 × 864) et `kirito arm` (1513 × 1686) ;
- positions « planche » = même formule pour `asuna body bottom` (1738 × 1719), `asuna body` (720 × 1156) et
  `kirito face` (852 × 726) ;
- chaque `animationlayers[].animation` de la scène correspond à un `id` du `.mdl` de l'image ;
- chaque `attachment` de la scène existe dans le `MDAT` de la marionnette du parent direct.

Les 28 `.mdl` statiques du dossier d'installation (`MDLV0004`, `0014`, `0017`, `0023`) n'ont aucune section
chaînée : tests négatifs de non-régression (pas de squelette, pas d'erreur).

---

## 11. Questions ouvertes

- **QO-P1 [Observé]** — `MDLS` : le bloc de 11 octets entre le dernier os et les boîtes est nul sur tout le corpus.
  Sa structure est inconnue (compteurs de contraintes, IK, physique ?). Lumen saute à la fin de section après les
  os, donc il n'en dépend pas.
- **QO-P2 [Observé]** — `cstr` après la matrice de chaque os, toujours vide. JSON de contraintes ou de physique
  (« Edit Constraints » [Doc WE]) ? Le lire comme `cstr`, et non comme `u8`, rend la lecture robuste à une
  chaîne non vide.
- **QO-P3 [Observé]** — `MDLA` : 35 octets nuls en fin d'animation et trois `u32` réservés (après `id`, après
  `N`, en tête de piste). Ils contiennent peut-être des pistes de « blend rules », de blend shapes ou des
  événements. Un fichier avec ces fonctions activées est nécessaire pour trancher.
- **QO-P4 [Doc WE]** — Physique des os (ressort / rigide, gravité, limites, cordes cinématiques depuis 2.6),
  cinématique inverse (cibles, « Wrap loop frames ») et « blend rules » (changement d'os parent pendant une
  animation). Le stockage est inconnu et le corpus n'en contient pas : non implémenté. La doc indique que la
  physique dépend du fps. À reprendre quand un échantillon sera disponible.
- **QO-P5 [Doc WE]** — Blend shapes (`getBlendShapeWeight`…, combos `MORPHING`, texture `g_Texture5`) : format
  inconnu, absent du corpus (aucun drapeau de sommet ni section associée observé).
- **QO-P6 [Inféré]** — Interpolation linéaire entre images précalculées, ou valeur en escalier ? Une capture WE
  d'une animation à 7,5 fps permettrait de trancher.
- **QO-P7 [Inféré]** — Chaînes exactes des modes `mirror` et `single` dans `MDLA` (seul `loop` observé).
- **QO-P8 [Inféré]** — Mode additif : écart au **repos** (choisi) ou à l'image 0 de l'animation ? Échelle
  additive (choisie) ou multiplicative ? Ce choix n'a d'effet qu'avec plusieurs calques sur les mêmes os.
- **QO-P9 [Inféré]** — Rampes `blendin` / `blendout` : forme (linéaire choisie), base de temps, application aux
  modes `loop` et `mirror`, effet de `rate`.
- **QO-P10 [Inféré]** — Sens de rotation des angles d'os (trigonométrique supposé) et unité côté SceneScript
  (degrés possibles, comme pour `ILayer.angles`).
- **QO-P11 [Inféré]** — Repère des transformations de `MDAT` : relatif à l'os (choisi ; seule lecture cohérente
  avec les positions de 3463520581) ou à l'EM. Rôle de l'alignement du parent. Décomposition de `A(t)` en cas
  d'échelle non uniforme combinée à une rotation (cisaillement perdu).
- **QO-P12 [Observé]** — Visibilité des enfants. Le fond 3463520581 bascule entre deux tailles de personnages
  avec une propriété utilisateur appliquée **seulement** aux marionnettes de contrôle (`visible` lié à
  `characterssize`). Les enfants n'ont pas de `visible` et portent `disablepropagation: false`. Sans propagation
  de la visibilité du parent, les deux personnages s'afficheraient en même temps. Cela contredit la non-propagation
  de la spec 02 (Q16). Il faut probablement propager la visibilité sauf si `disablepropagation` est vrai. À
  trancher dans la spec 02.
- **QO-P13 [Inféré]** — Comportement de WE quand le shader de la passe 0 n'a pas de chemin `SKINNING` (shader
  workshop personnalisé) : maillage figé ou skinning appliqué quand même ? Lumen fait un skinning CPU.
- **QO-P14 [Inféré]** — Valeur de `BONECOUNT` dans WE (nombre d'os, ou maximum fixe) et nombre maximal d'os.
  Tenir compte de la limite d'uniforms du GPU.
- **QO-P15 [Observé]** — Champs non interprétés de `MDLV` : drapeaux du modèle `0x01800009`, `u32` réservé = 1,
  `u32` = 0 et boîte nulle par maillage, `u32` = 1 du bloc de mise en page, `u32` = 0 en fin de queue.
- **QO-P16 [Inféré]** — `MDLV0021` (acceptée par l'amont) : même disposition que `0023` ? Autres versions de
  `MDLS`, `MDAT`, `MDLA`, `MDLE` (antérieures ou postérieures) : dispositions inconnues, donc sections ignorées.
- **QO-P17 [Inféré]** — Sommet dont tous les poids sont nuls (effondré sur l'origine, parité shader) ou poids sur
  un indice hors bornes (Lumen rejette le squelette) : absents du corpus.
- **QO-P18 [Inféré]** — Horloge des calques : avance pendant `visible` = faux ? Comportement après une pause plein
  écran (Lumen exclut la durée de pause, alors que `g_Time` saute dans la spec 03) ? Valeur par défaut de
  `visible` quand la clé manque (faux selon la spec 02, vrai dans toutes les données).
- **QO-P19 [Doc WE / Observé]** — `SKINNING_ALPHA` / `g_BonesAlpha` existent dans les shaders, mais aucune
  donnée d'alpha par os n'apparaît dans les échantillons de 36 octets. Source inconnue : alpha 1 supposé.
- **QO-P20 [Doc WE]** — Rognage : la déformation hors de S est coupée quand l'image a des effets (composite A).
  Avec une passe unique redirigée (spec 04 Q22), WE coupe-t-il aussi ?
- **QO-P21 [Inféré]** — `getBoneIndex` avec des noms vides ou en double. Comportement de `getBoneParentIndex`, dont
  la description dans la doc WE est copiée sur celle de `getBoneIndex`.
- **QO-P22 [Doc WE]** — Effets dont un point est lié à un attachement (« Bind to Attachment », ex. émetteurs de
  l'Advanced Fluid Simulation, actif seulement à l'exécution) : à spécifier avec les effets concernés, en
  réutilisant `A(t)` de §9.10.

## Amendements proposés aux autres specs

Ces amendements sont proposés à leurs rédacteurs ; ils ne sont pas appliqués par ce document.

- **Spec 01 §3.1** : renvoyer à la lecture structurée de §2 et au chaînage de §3. L'heuristique devient le repli
  pour les versions autres que `MDLV0023`.
- **Spec 02 §7.2 / §7.5** : clé d'objet `attachment` ; clés de calque `name`, `additive`, `blendin`, `blendout`,
  `blendtime` ; QO-P12 (propagation de la visibilité, `disablepropagation`).
- **Spec 03 §3.4 / §5.4** : attributs `a_BlendIndices` (entier), `a_BlendWeights`, `a_Normal`, `a_Tangent4` pour les
  marionnettes ; combos moteur `SKINNING` et `BONECOUNT` ; uniform `g_Bones` (`mat4x3[]`).
- **Spec 04 §8.1 / §20** : nœud d'attachement (§9.10), positions skinnées (§9.8), ordre par frame (§9.11), Q22.
- **Spec 06** : API des os et des calques d'animation (§9.12).
