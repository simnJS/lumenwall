# Spec 01 — Formats binaires, système de fichiers virtuel et résolution des assets

Statut : rédigée en clean-room à partir de l'observation du comportement amont. Aucune ligne de code amont n'est reproduite.
Public : implémenteurs Rust de Lumen (crates `lumen-formats`, `lumen-platform`, `lumen-render`, `lumen-video`).

Périmètre :

1. l'archive `.pkg` ;
2. le format de texture `.tex` (blocs TEXV / TEXI / TEXB / TEXS, formats de pixels, drapeaux, LZ4, mipmaps, images embarquées, vidéo embarquée, animations) et sa conversion en texture GPU ;
3. les autres formats binaires lus (maillage de « marionnette » `MDLV`, formats délégués à des décodeurs tiers) ;
4. le système de fichiers virtuel : points de montage, adaptateurs, normalisation et ordre de résolution des chemins ;
5. la localisation des dossiers Steam (workshop, installation, `assets`) ;
6. des fixtures de test fabriquées octet par octet ;
7. les questions ouvertes.

Tout ce qui concerne la sémantique des JSON (`project.json`, scènes, matériaux, effets) est hors périmètre, sauf quand un JSON pilote la lecture d'un binaire.

---

## 0. Conventions

### 0.1 Types primitifs

| Notation | Taille | Description |
|---|---|---|
| `u8` | 1 | octet non signé |
| `u16` | 2 | entier non signé, **little-endian** |
| `u32` | 4 | entier non signé, **little-endian** |
| `i32` | 4 | entier signé complément à deux, **little-endian** |
| `f32` | 4 | flottant IEEE-754 simple précision, **little-endian** |
| `sstr` | 4 + n | « chaîne dimensionnée » : un `u32` n (longueur en octets) suivi de n octets bruts. **Pas** de terminateur nul. |
| `cstr` | n + 1 | « chaîne C » : octets lus jusqu'au premier `0x00` inclus. Le `0x00` est consommé mais ne fait pas partie de la valeur. |
| `magic9` | 9 | 8 caractères ASCII suivis d'un octet `0x00`. La comparaison porte sur les **9** octets : le 9ᵉ octet DOIT valoir `0x00`. |

Tous les entiers et flottants de tous les formats de ce document sont little-endian, quel que soit l'hôte.

### 0.2 Chaînes

Les octets des `sstr` et `cstr` sont conservés tels quels (aucune conversion d'encodage, aucune normalisation Unicode). En pratique ils sont en UTF-8 / ASCII. Les comparaisons sont **octet à octet** et **sensibles à la casse**.

### 0.3 Fin de fichier

L'amont ne détecte pas les lectures au-delà de la fin du flux : il continue avec des valeurs indéterminées (voir QO-01). Lumen DOIT lever une erreur `FinDeFluxInattendue` dès qu'une lecture dépasse la fin des données disponibles. Ce comportement est le seul choix déterministe raisonnable. Les fixtures négatives du §7 le testent.

### 0.4 Catégories d'erreurs

La spec distingue deux familles d'erreurs, car l'amont les traite différemment lors des replis (voir §4.9 et §4.11) :

- **`Introuvable`** : le chemin ne se résout dans aucun point de montage, un adaptateur refuse le chemin, ou une opération n'est pas prise en charge par l'adaptateur (chemin physique dans une archive par exemple).
- **`Invalide`** : le fichier existe mais son contenu est incorrect (magic inattendu, valeur d'enum inconnue, échec LZ4, fin de flux…), ou une erreur d'E/S survient.

Les codes d'erreur nommés dans ce document (`E_PKG_…`, `E_TEX_…`) sont des propositions pour Lumen. L'amont produit des messages texte libres qui contiennent la valeur fautive. Lumen DOIT inclure la valeur fautive dans le message (magic lu, valeur numérique…).

---

## 1. Archive `.pkg`

### 1.1 Rôle

Un `.pkg` regroupe les fichiers d'un fond d'écran (scène JSON, matériaux, textures `.tex`, shaders, sons…) dans un seul fichier. Deux noms sont recherchés dans le dossier du fond d'écran : `scene.pkg` et `gifscene.pkg` (voir §4.8). Un `.pkg` peut aussi être passé directement comme chemin du fond d'écran (voir §4.1.2).

### 1.2 Disposition binaire

| Ordre | Champ | Type | Description |
|---|---|---|---|
| 1 | en-tête | `sstr` | Signature de version. DOIT **commencer** par les 4 octets ASCII `PKGV` (`50 4B 47 56`). En pratique : `PKGV` + 4 chiffres, par exemple `PKGV0001` ou `PKGV0019`. La partie après `PKGV` n'est **ni interprétée ni validée**. |
| 2 | nombre d'entrées N | `u32` | Nombre d'entrées dans la table. |
| 3 | table d'entrées | N × entrée | Voir ci-dessous. |
| 4 | zone de données | octets | Contenu concaténé des fichiers. |

Disposition d'une entrée de table :

| Ordre | Champ | Type | Description |
|---|---|---|---|
| 1 | nom | `sstr` | Chemin relatif du fichier dans l'archive. Séparateur `/`, pas de `/` initial (exemple : `materials/foo.tex`). |
| 2 | offset | `u32` | Décalage du début des données, **relatif au début de la zone de données** (et non au début du fichier). |
| 3 | taille | `u32` | Nombre d'octets du fichier. |

### 1.3 Calcul des positions

- `base` = position (en octets depuis le début du fichier) qui suit immédiatement la dernière entrée de la table, donc la fin de la table.
- Les données de l'entrée *e* occupent `[base + e.offset, base + e.offset + e.taille)`.
- Les entrées ne sont pas forcément contiguës ni ordonnées. L'amont ne vérifie ni le chevauchement ni le débordement (QO-02).

### 1.4 Validation à l'ouverture

1. Lire l'en-tête `sstr`. S'il ne commence pas par `PKGV` : erreur `E_PKG_ENTETE` (catégorie `Invalide`). Une longueur 0, ou une longueur inférieure à 4, donne aussi `E_PKG_ENTETE`.
2. Lire N puis les N entrées. Aucune autre validation n'est faite par l'amont.
3. Calculer `base`.

Recommandations pour Lumen (robustesse, sans changer le résultat sur une archive valide) : refuser une longueur de `sstr` supérieure à la taille restante du fichier (`FinDeFluxInattendue`), et refuser une entrée dont `base + offset + taille` dépasse la taille du fichier (`E_PKG_ENTREE_HORS_LIMITES`). Ces deux contrôles sont des divergences volontaires (QO-02).

### 1.5 Recherche d'une entrée

- Clé de recherche : le chemin normalisé (§4.6) **sans** `/` initial, comparé octet à octet au nom de l'entrée.
- Sensible à la casse. Aucun traitement des `\`.
- Si plusieurs entrées portent le même nom, **la première dans l'ordre de la table** gagne.
- Recherche linéaire dans l'amont. Lumen peut indexer (table de hachage) à condition de conserver la règle « première occurrence ».
- Un nom absent rend l'entrée inexistante pour l'archive. La résolution passe alors au point de montage suivant (§4.7).

### 1.6 Lecture

L'ouverture d'une entrée copie ses `taille` octets en mémoire et renvoie un flux mémoire positionné au début. Ce flux prend en charge le positionnement absolu, relatif et depuis la fin. L'archive reste ouverte pendant toute la durée de vie du montage, et chaque ouverture relit le disque.

Note d'implémentation : l'amont partage un seul descripteur de fichier entre toutes les lectures. Lumen DOIT utiliser des lectures positionnelles (`pread`/`seek_read`) ou un mmap pour rester sûr en concurrence.

---

## 2. Format de texture `.tex`

### 2.1 Vue d'ensemble

Un fichier `.tex` est une suite de blocs lus séquentiellement, sans table d'offsets :

```
[TEXV0005]  en-tête global
[TEXI0001]  description de la texture (format, drapeaux, dimensions)
[TEXB000n]  conteneur d'images (n = 1..4)
  image 0 : nombre de mipmaps, puis les mipmaps
  image 1 : …
  …
[TEXS000m]  (présent seulement si le drapeau « animé » est levé) table de frames
```

Toute donnée qui suit le dernier bloc attendu est ignorée.

### 2.2 En-tête TEXV + TEXI (offsets absolus fixes)

| Offset | Taille | Champ | Type | Règle |
|---|---|---|---|---|
| 0x00 | 9 | magic global | `magic9` | DOIT être `TEXV0005\0`, sinon `E_TEX_TEXV`. |
| 0x09 | 9 | magic de description | `magic9` | DOIT être `TEXI0001\0`, sinon `E_TEX_TEXI`. |
| 0x12 | 4 | format de pixels | `u32` | Voir §2.3. Valeur hors liste : `E_TEX_FORMAT`. |
| 0x16 | 4 | drapeaux | `u32` | Voir §2.4. Valeur refusée : `E_TEX_DRAPEAUX`. |
| 0x1A | 4 | largeur de texture `LT` | `u32` | Largeur de la surface allouée (souvent une puissance de 2 ou un multiple de 4). |
| 0x1E | 4 | hauteur de texture `HT` | `u32` | Hauteur de la surface allouée. |
| 0x22 | 4 | largeur d'image `LI` | `u32` | Largeur utile du contenu (≤ `LT` en pratique). |
| 0x26 | 4 | hauteur d'image `HI` | `u32` | Hauteur utile du contenu. |
| 0x2A | 4 | réservé | `u32` | Lu et ignoré. |
| 0x2E | 9 | magic du conteneur | `magic9` | `TEXB0001\0` … `TEXB0004\0`. Voir §2.5. |
| 0x37 | 4 | nombre d'images `NI` | `u32` | Lu **avant** la validation du magic du conteneur. |

Les deux magics sont validés dans l'ordre (TEXV puis TEXI), puis le format, puis les drapeaux. La première erreur rencontrée interrompt la lecture.

### 2.3 Énumération des formats de pixels

Le champ peut prendre les valeurs ci-dessous. Toute autre valeur (3, 5, 16 et plus, sauf `0xFFFFFFFF`) donne `E_TEX_FORMAT` dès la lecture.

La colonne « GPU » indique si le format peut être converti en texture GPU (§2.10) **quand la texture contient des pixels bruts**, c'est-à-dire sans image embarquée et sans vidéo MP4. Dans les autres cas le format est ignoré et la texture GPU est en RGBA8.

| Valeur | Nom usuel | Disposition des données brutes | GPU | Format GPU |
|---|---|---|---|---|
| `0xFFFFFFFF` | inconnu | — | non | — |
| 0 | « ARGB8888 » | 4 octets/pixel, **ordre mémoire R, G, B, A** (le nom est trompeur) | oui | RGBA8 UNORM (non sRGB) |
| 1 | RGB888 | 3 o/px (informatif) | non | — |
| 2 | RGB565 | 2 o/px (informatif) | non | — |
| 4 | DXT5 | blocs 4×4 de 16 octets | oui | BC3 RGBA UNORM |
| 6 | DXT3 | blocs 4×4 de 16 octets | oui | BC2 RGBA UNORM |
| 7 | DXT1 | blocs 4×4 de 8 octets | oui | BC1 **RGBA** UNORM (alpha 1 bit) |
| 8 | RG88 | 2 o/px, ordre R, G | oui | RG8 UNORM |
| 9 | R8 | 1 o/px | oui | R8 UNORM |
| 10 | RG1616f | 4 o/px (informatif) | non | — |
| 11 | R16f | 2 o/px (informatif) | non | — |
| 12 | BC7 | blocs 4×4 de 16 o (informatif) | non | — |
| 13 | RGBa1010102 | 4 o/px (informatif) | non | — |
| 14 | RGBA16161616f | 8 o/px (informatif) | non | — |
| 15 | RGB161616f | 6 o/px (informatif) | non | — |

Taille attendue d'un niveau compressé par blocs (informatif, l'amont ne la vérifie pas) : `max(1, ceil(l/4)) × max(1, ceil(h/4)) × octets_par_bloc`.

Les formats marqués « non » passent l'analyse sans erreur. L'erreur `E_TEX_FORMAT_GPU` (catégorie `Invalide`) n'apparaît qu'à la création de la texture GPU, et seulement si aucune image n'est embarquée (§2.10).

### 2.4 Drapeaux

| Bit (valeur) | Nom proposé pour Lumen | Effet observé |
|---|---|---|
| 0x1 (1) | `SANS_INTERPOLATION` | Filtrage au plus proche (§2.10.4). |
| 0x2 (2) | `UV_BORNES` | Adressage « clamp to edge » sur les deux axes. Sans ce bit : répétition. |
| 0x4 (4) | `ANIME` | La texture est animée : un bloc TEXS suit les images (§2.8). |
| 0x8 (8) | `UV_BORNES_BORDURE` | **Ignoré** pour les textures `.tex` (QO-05). |
| 0x20 (32) | `VIDEO` | La texture est une vidéo (§2.10.2). |
| 0x80000 (524288) | `PRIORITE_ALPHA` | Lu, jamais exploité. Indique que l'alpha est porté par le canal G (RG88) ou R (R8). |

**Règle de validation exacte :** la valeur est acceptée si et seulement si elle est **strictement inférieure à 524335** (`0x8002F`, la somme de tous les bits ci-dessus). C'est une comparaison numérique, pas un masque. Conséquences à reproduire telles quelles (QO-04) :

- les bits non documentés (16, 64, 128…) sont acceptés tant que la valeur reste < 524335 ;
- la combinaison de **tous** les bits connus (524335) est refusée ;
- toute valeur ≥ 524335 est refusée (par exemple `0x100000`).

Une valeur refusée donne `E_TEX_DRAPEAUX`.

### 2.5 Conteneur TEXB

Après le `magic9` du conteneur (0x2E) et `NI` (0x37), le contenu dépend de la version :

| Magic lu | Champs supplémentaires après `NI` | Version effective |
|---|---|---|
| `TEXB0001\0` | aucun | 1 |
| `TEXB0002\0` | aucun | 2 |
| `TEXB0003\0` | `u32` type d'image embarquée `TIE` (§2.6) | 3 |
| `TEXB0004\0` | `u32` `TIE`, puis `u32` indicateur MP4 | 4 ou 3, voir ci-dessous |
| autre | — | erreur `E_TEX_TEXB` |

Règles propres à `TEXB0004` :

1. `est_mp4` = (indicateur MP4 == 1). Toute autre valeur vaut « faux ».
2. Si `TIE` vaut « inconnu » (`0xFFFFFFFF`) **et** `est_mp4` : `TIE` reçoit la valeur 35, qui désigne la vidéo MP4.
3. Si `TIE` ≠ 35 après l'étape 2, la version effective **redescend à 3** : les mipmaps sont lues au format v3. Sinon la version effective reste 4.

Conséquence : un `TEXB0004` avec `TIE = 35` (WEBP selon l'énumération) reste en version effective 4 même si `est_mp4` est faux (QO-06).

Valeurs par défaut quand le champ est absent : `TIE` = inconnu pour v1 et v2 ; `est_mp4` = faux hors v4.

Position du premier octet des images : 0x3B en v1 et v2, 0x3F en v3, 0x43 en v4 (que la version effective soit 3 ou 4).

### 2.6 Type d'image embarquée (`TIE`)

Les valeurs reprennent la numérotation des formats de la bibliothèque FreeImage. Toute valeur hors liste donne `E_TEX_TIE`.

| Valeur | Format | Valeur | Format |
|---|---|---|---|
| −1 (`0xFFFFFFFF`) | inconnu : pas d'image embarquée | 18 | TIFF |
| 0 | BMP | 19 | WBMP |
| 1 | ICO | 20 | PSD |
| 2 | JPEG | 21 | CUT |
| 3 | JNG | 22 | XBM |
| 4 | KOALA | 23 | XPM |
| 5 | LBM / IFF | 24 | DDS |
| 6 | MNG | 25 | GIF |
| 7 | PBM | 26 | HDR |
| 8 | PBMRAW | 27 | FAXG3 |
| 9 | PCD | 28 | SGI |
| 10 | PCX | 29 | EXR |
| 11 | PGM | 30 | J2K |
| 12 | PGMRAW | 31 | JP2 |
| 13 | PNG | 32 | PFM |
| 14 | PPM | 33 | PICT |
| 15 | PPMRAW | 34 | RAW |
| 16 | RAS | 35 | WEBP **ou** MP4 (§2.5) |
| 17 | TARGA | 36 | JXR |

Interprétation :

- **Seule la distinction « inconnu / connu » compte pour le décodage.** Dès que `TIE` ≠ inconnu, chaque mipmap est un fichier image complet (PNG, JPEG…). Le décodeur choisit le format **par reniflage du contenu** et ignore la valeur de `TIE`.
- Le décodeur de l'amont reconnaît JPEG (baseline et progressif), PNG (y compris 16 bits, ramené à 8 bits), BMP, TGA, PSD (image composite), GIF (**première frame uniquement**), HDR (converti en 8 bits), PIC (Softimage) et PNM (PGM/PPM binaires). Il ne reconnaît **pas** WEBP, TIFF, DDS, EXR, ICO, JP2/J2K, JXR, etc. Pour être fidèle, Lumen DOIT reconnaître au moins cet ensemble. Il PEUT en reconnaître davantage (QO-07).
- La sortie du décodage est toujours forcée en **RGBA 8 bits**. La largeur et la hauteur décodées remplacent celles de l'en-tête de mip pour l'envoi au GPU, mais **pas** pour les calculs de dimensions du §2.11.
- Aucune correction d'orientation EXIF, aucun retournement vertical, aucune prémultiplication d'alpha.
- Si le décodage échoue, la texture GPU est allouée aux dimensions de l'en-tête de mip avec un contenu indéterminé (en pratique noir transparent), et l'erreur passe inaperçue. Lumen DOIT remplir de zéros (RGBA = 0,0,0,0) et journaliser un avertissement (QO-07).
- Valeur 35 avec `est_mp4` : vidéo (§2.10.2).

### 2.7 Images et mipmaps

Pour chaque image `i` de 0 à `NI − 1`, dans l'ordre :

| Ordre | Champ | Type |
|---|---|---|
| 1 | nombre de mipmaps `NM` | `u32` |
| 2 | `NM` mipmaps | voir ci-dessous |

Le niveau 0 est le plus grand. Les niveaux sont stockés dans l'ordre croissant d'indice. `NM` peut valoir 0 (QO-09).

#### 2.7.1 Disposition d'une mipmap selon la version effective

**Version 1 (`TEXB0001`) :**

| Ordre | Champ | Type |
|---|---|---|
| 1 | largeur | `u32` |
| 2 | hauteur | `u32` |
| 3 | taille stockée `TS` | `i32` |
| 4 | données | `TS` octets bruts |

Il n'y a pas de champ de compression : la mipmap est réputée non compressée.

**Versions 2 et 3 (y compris `TEXB0004` redescendu en 3) :**

| Ordre | Champ | Type |
|---|---|---|
| 1 | largeur | `u32` |
| 2 | hauteur | `u32` |
| 3 | compression `C` | `u32` |
| 4 | taille décompressée `TD` | `i32` |
| 5 | taille stockée `TS` | `i32` |
| 6 | données | voir §2.7.2 |

**Version effective 4 (MP4 / `TIE` = 35) :**

| Ordre | Champ | Type |
|---|---|---|
| 1 | réservé A | `u32`, ignoré |
| 2 | réservé B | `u32`, ignoré |
| 3 | métadonnées | `cstr`, texte JSON conservé mais inexploité |
| 4 | réservé C | `u32`, ignoré |
| 5…10 | identiques aux champs 1 à 6 des versions 2/3 | |

#### 2.7.2 Compression et tailles

Valeurs par défaut quand le champ est absent (v1) : `C` = 0 et `TD` = `TS`.

| `C` | Lecture | Taille finale des données |
|---|---|---|
| 0 | Lire `TS` octets bruts. `TD` lu dans le fichier est **écrasé** par `TS`. | `TS` |
| 1 | Lire `TS` octets formant un **bloc LZ4 brut** (format « block », sans en-tête de trame LZ4 ni somme de contrôle), puis le décompresser dans un tampon de `TD` octets. | `TD` |
| autre (≥ 2) | Lire **`TD`** octets bruts (et non `TS`). | `TD` (QO-08) |

Règles LZ4 :

- Le décompresseur DOIT refuser toute écriture au-delà de `TD` octets et toute lecture au-delà de `TS` octets (équivalent d'un décodage « sûr »).
- Si le décodage signale une erreur : `E_TEX_LZ4` (catégorie `Invalide`), et l'analyse de toute la texture échoue.
- L'amont ne vérifie pas que le nombre d'octets produits est égal à `TD`. Les octets restants sont alors indéterminés. Lumen DOIT les mettre à zéro et journaliser un avertissement (QO-08).

Tailles négatives (`TS` ou `TD` < 0) : comportement indéfini dans l'amont (tentative d'allocation géante). Lumen DOIT lever `E_TEX_TAILLE`.

Organisation des pixels bruts : lignes contiguës, **sans aucun remplissage de fin de ligne**, première ligne du fichier envoyée en première ligne de la texture GPU (pas de retournement). Voir QO-10 pour l'alignement.

### 2.8 Bloc d'animation TEXS

Le bloc est présent si et seulement si le bit `ANIME` (0x4) des drapeaux est levé. Il commence immédiatement après la dernière mipmap de la dernière image.

| Ordre | Champ | Type | Condition |
|---|---|---|---|
| 1 | magic | `magic9` | `TEXS0001\0`, `TEXS0002\0` ou `TEXS0003\0`. Autre valeur : `E_TEX_TEXS`. |
| 2 | nombre de frames `NF` | `u32` | toujours |
| 3 | largeur d'animation `LA` | `u32` | TEXS0003 seulement |
| 4 | hauteur d'animation `HA` | `u32` | TEXS0003 seulement |
| 5 | `NF` frames | 32 octets chacune | voir ci-dessous |

Une frame compte 8 champs de 4 octets. On les note F0 à F7 :

| Champ | TEXS0001 | TEXS0002 / TEXS0003 | Signification |
|---|---|---|---|
| F0 | `u32` | `u32` | indice de l'image (0..`NI`−1) affichée par la frame |
| F1 | `f32` | `f32` | durée d'affichage en secondes |
| F2 | `u32`, converti en flottant | `f32` | x : origine dans l'image, en pixels |
| F3 | `u32`, converti en flottant | `f32` | y : origine dans l'image, en pixels |
| F4 | `u32`, converti en flottant | `f32` | composante x du vecteur « largeur » du rectangle de la frame (`wx`) |
| F5 | `u32` **ignoré** (vaut 0) | `f32` | composante y du vecteur « largeur » (`wy`) |
| F6 | `u32` **ignoré** (vaut 0) | `f32` | composante x du vecteur « hauteur » (`hx`) |
| F7 | `u32`, converti en flottant | `f32` | composante y du vecteur « hauteur » (`hy`) |

Le couple (`wx`,`wy`) et le couple (`hx`,`hy`) décrivent les deux côtés d'un rectangle éventuellement pivoté dans l'atlas. Un rectangle droit a `wy = hx = 0`, `wx` = largeur et `hy` = hauteur.

Dimensions d'animation :

- TEXS0003 : `LA` et `HA` sont lus dans le bloc.
- TEXS0001 et TEXS0002 : `LA` = F4 de la **première** frame et `HA` = F7 de la première frame, convertis en entier non signé par troncature vers zéro. Si `NF` = 0, l'amont a un comportement indéfini. Lumen DOIT alors fixer `LA = HA = 0` (QO-11).

### 2.9 Données de planche de sprites (spritesheet)

Quatre valeurs dérivées sont attachées à la texture : `colonnes`, `lignes`, `frames_planche` et `durée_planche`. Elles valent 0 par défaut, et 0.0 pour la durée. La texture « a une planche » si `frames_planche > 0`. Ces valeurs sont exploitées par les particules (autre spec).

#### 2.9.1 Inférence à partir de TEXS

On l'applique après la lecture d'un bloc TEXS si `NF > 0`, `LI > 0` et `HI > 0` :

1. `lf` = F4 de la première frame et `hf` = F7 de la première frame (flottants).
2. Si `lf > 0` et `hf > 0` :
   - `col = round(LI / lf)` et `lig = round(HI / hf)`, en double précision, arrondi au plus proche avec les demis éloignés de zéro, puis conversion en entier non signé ;
   - si `col > 0`, `lig > 0` et `col × lig ≥ NF`, alors `colonnes = col`, `lignes = lig`, `frames_planche = NF` et `durée_planche` = somme des F1 de toutes les frames.
3. Sinon, rien n'est modifié.

Cette règle évite de traiter une animation de type GIF, où chaque frame est une image entière, comme une planche 1×1.

#### 2.9.2 Fichier compagnon `.tex-json`

Après l'analyse binaire, quand la texture est chargée sous le nom logique `<nom>` (§4.10), on tente de lire `materials/<nom>.tex-json` avec la résolution complète du §4.7, puis de l'analyser en JSON.

- Si la clé `spritesheetsequences` existe, est un tableau et n'est pas vide, on prend son **premier** élément et on y lit :
  - `frames` : entier, défaut 0 ;
  - `width` : flottant, défaut 0.0 ;
  - `height` : flottant, défaut 0.0 ;
  - `duration` : flottant, défaut **1.0**.
- Si `frames > 0`, `width > 0`, `height > 0`, `LI > 0` et `HI > 0`, alors `colonnes = round(LI / width)`, `lignes = round(HI / height)`, `frames_planche = frames` et `durée_planche = duration`. Ces valeurs **écrasent** celles de §2.9.1. Ici aucune vérification `col × lig ≥ frames` n'est faite.
- **Toute** erreur est silencieusement ignorée : fichier absent, JSON invalide, type incompatible (par exemple `"frames": "3"`). Dans ce cas les valeurs de §2.9.1 sont conservées. Un nombre flottant fourni pour `frames` est tronqué en entier.

### 2.10 Conversion en texture GPU

#### 2.10.1 Ordre des opérations

1. Calculer le vecteur de résolution (§2.11).
2. Déterminer le format GPU. Si `TIE` ≠ inconnu, le format est RGBA8. Sinon on applique la table du §2.3, et un format non pris en charge donne `E_TEX_FORMAT_GPU`. **Cette étape a lieu avant le test vidéo** : une texture vidéo sans image embarquée dont le format de pixels n'est pas pris en charge échoue elle aussi (QO-12).
3. Si `est_mp4` ou si le drapeau `VIDEO` est levé : chemin vidéo (§2.10.2).
4. Sinon : chemin image (§2.10.3).

#### 2.10.2 Chemin vidéo

- Il faut au moins une image avec au moins une mipmap, sinon `E_TEX_VIDEO_VIDE`.
- Une seule texture GPU est créée. Son échantillonneur est configuré selon §2.10.4 avec le nombre de mipmaps de l'image 0.
- Les octets décompressés de la mipmap 0 de l'image 0 forment un fichier vidéo complet (MP4 en pratique), lu **depuis la mémoire**.
- Taille de la surface de sortie : `LT × HT`.
- Lecture muette (volume 0), non cadencée sur l'horloge audio, démarrée seulement quand au moins un consommateur utilise la texture. Le détail figure dans la spec vidéo.
- Les autres images et mipmaps sont ignorées.

#### 2.10.3 Chemin image

- Une texture GPU par image (`NI` textures). L'image `i` reçoit l'indice de texture `i`.
- Pour chaque image, chaque mipmap `k` (dans l'ordre du fichier) est envoyée au niveau de mip `k` :
  - si `TIE` ≠ inconnu : décodage de l'image embarquée en RGBA8 (§2.6), puis envoi aux dimensions décodées ;
  - sinon, pour RGBA8, RG8 et R8 : envoi des octets bruts aux dimensions de l'en-tête de mip ;
  - sinon, pour BC1, BC2 et BC3 : envoi compressé, avec une taille de données égale à la taille finale du §2.7.2.
- Plage de niveaux de mip : de 0 à `NM(i) − 1`.

#### 2.10.4 Échantillonnage

| Paramètre | Valeur |
|---|---|
| Adressage U et V | `UV_BORNES` levé : clamp to edge. Sinon : répétition. `UV_BORNES_BORDURE` n'a **aucun** effet ici. |
| Filtre d'agrandissement | `SANS_INTERPOLATION` levé : au plus proche. Sinon : linéaire. |
| Filtre de réduction | `SANS_INTERPOLATION` levé : au plus proche, et mip au plus proche. Sinon : linéaire, et mip linéaire (trilinéaire). |
| Anisotropie | 8 |
| Espace colorimétrique | linéaire (UNORM). Aucune texture `.tex` n'est traitée comme sRGB. |

Les surfaces de rendu intermédiaires créées pour un objet image héritent des drapeaux de sa texture. Pour elles, `UV_BORNES_BORDURE` (sans `UV_BORNES`) produit un adressage « clamp to border ». Cette règle relève de la spec de rendu ; elle est rappelée ici pour souligner l'asymétrie.

#### 2.10.5 Interaction avec les shaders

Quand la texture d'entrée principale d'une passe est au format RG88, le combo de shader `TEX0FORMAT` est forcé à 8. Pour R8, il est forcé à 9. Pour les autres formats, le combo n'est pas touché. Le détail est dans la spec shaders.

### 2.11 Dimensions : texture et image

Définitions utilisées par le rendu :

- **Taille réelle** (`LR`, `HR`) : (`LA`, `HA`) si la texture est animée, sinon (`LI`, `HI`).
- **Taille de texture de l'image `i`** : largeur et hauteur de l'**en-tête** de la mipmap 0 de l'image `i` si `i < NI`, sinon (`LT`, `HT`).
- **Vecteur de résolution** (vec4), transmis aux uniforms `g_Texture0Resolution`, `g_Texture1Resolution`, etc. :

| Cas | Vecteur |
|---|---|
| texture animée | (`LT`, `HT`, `LA`, `HA`) |
| image embarquée (`TIE` ≠ inconnu), non animée | (largeur d'en-tête mip0 de l'image 0, hauteur d'en-tête mip0 de l'image 0, `LI`, `HI`) |
| autres cas | (`LT`, `HT`, `LI`, `HI`) |

- **Zone utile en coordonnées UV** pour un objet image : u ∈ [0, `LR` / largeur de texture de l'image 0] et v ∈ [0, `HR` / hauteur de texture de l'image 0]. Le contenu utile est donc **en haut à gauche** de la surface allouée (premières colonnes et premières lignes des données), et le reste est du remplissage.
- Taille par défaut d'un objet image sans taille explicite : la taille réelle.

### 2.12 Animation : temporisation et sélection de frame

- Durée totale `T` = somme des F1 de toutes les frames. Elle est calculée une fois par objet consommateur.
- À chaque rendu, avec `t_rendu` le temps de rendu en secondes depuis le démarrage : `t = fmod(t_rendu, T)`, calculé en double précision.
- On parcourt les frames dans l'ordre du fichier en retranchant à chaque fois F1 de `t`. La **première** frame après laquelle `t ≤ 0` est retenue. Avec deux frames de 0,5 s, `t = 0,5` sélectionne donc la frame 0.
- Si `T = 0`, le `fmod` donne NaN, aucune comparaison « > 0 » n'est vraie, et c'est la première frame qui est retenue.
- Si aucune frame n'est retenue, l'état par défaut s'applique : image 0, translation (0,0), rotation (0,0,0,0).
- Pour la frame retenue, avec `img` = F0, `L` = largeur de texture de `img` et `H` = hauteur de texture de `img` (§2.11) :
  - on lie l'image `img` sur l'unité de texture 0, ou l'image 0 si `img ≥ NI` ;
  - uniform `g_Texture0Translation` (vec2) = (F2 / L, F3 / H) ;
  - uniform `g_Texture0Rotation` (vec4) = (F4 / L, F5 / L, F6 / H, F7 / H).
- Hors animation, ces deux uniforms reçoivent des zéros.

### 2.13 Récapitulatif des erreurs `.tex`

| Code | Cause | Moment |
|---|---|---|
| `E_TEX_TEXV` | magic ≠ `TEXV0005\0` | analyse |
| `E_TEX_TEXI` | magic ≠ `TEXI0001\0` | analyse |
| `E_TEX_FORMAT` | format hors liste (§2.3) | analyse |
| `E_TEX_DRAPEAUX` | drapeaux ≥ 524335 | analyse |
| `E_TEX_TEXB` | magic de conteneur inconnu | analyse |
| `E_TEX_TIE` | `TIE` hors liste (§2.6) | analyse |
| `E_TEX_LZ4` | échec du décodage LZ4 | analyse |
| `E_TEX_TAILLE` | taille négative (spécifique à Lumen) | analyse |
| `E_TEX_TEXS` | magic d'animation inconnu | analyse |
| `FinDeFluxInattendue` | troncature (spécifique à Lumen) | analyse |
| `E_TEX_FORMAT_GPU` | format brut non pris en charge par le GPU | création GPU |
| `E_TEX_VIDEO_VIDE` | vidéo sans mipmap | création GPU |

Toutes sont de catégorie `Invalide`. Dans l'amont, elles **ne sont pas** rattrapées par la boucle de repli multi-fonds (§4.11) : elles remontent comme erreurs fatales.

---

## 3. Autres formats binaires

### 3.1 Maillage de marionnette (`MDLV`)

#### 3.1.1 Contexte

Un fichier JSON de modèle (référencé par un objet image) peut contenir la clé `puppet`, une chaîne qui donne le chemin d'un fichier binaire de modèle (généralement `.mdl`). Ce chemin est résolu par le §4.7, sans préfixe. Quand le chargement réussit, l'objet image est dessiné avec ce maillage au lieu du quadrilatère standard. **Tout échec est non fatal** : il est journalisé et l'objet garde le quadrilatère standard.

L'amont ne fait **pas** une analyse structurée du format. Il applique la recherche heuristique décrite ci-dessous. Lumen DOIT appliquer exactement la même heuristique pour obtenir le même maillage sur les mêmes fichiers (QO-15).

#### 3.1.2 Algorithme

Le fichier entier est lu en mémoire ; on note `D` ses octets et `|D|` sa taille.

1. **Version.** Si `|D| < 9`, ou si les **8 premiers** octets ne valent ni `MDLV0021` ni `MDLV0023` : échec (« version de marionnette non prise en charge »). Le 9ᵉ octet n'est pas vérifié.
2. **Limite `L`.** On cherche la première position `p ≥ 9` telle que `p + 4 < |D|` (inégalité stricte) et `D[p..p+4] = "MDLS"`. Si on en trouve une, `L = p`. Sinon `L = |D|`.
3. **Bloc de maillage.** Pour `p` = 9, 10, 11, … tant que `p + 12 < L` :
   - `vb` = `u32` lu à `p + 4` (le `u32` en `p` est ignoré) ;
   - on passe au `p` suivant si `vb = 0`, si `vb mod 80 ≠ 0` ou si `p + 8 + vb + 4 > L` ;
   - `ib` = `u32` lu à `p + 8 + vb` ;
   - on passe au `p` suivant si `ib = 0`, si `ib mod 6 ≠ 0` ou si `p + 8 + vb + 4 + ib > L` ;
   - sinon le bloc est trouvé à `p` ; on arrête.
   Si aucun `p` ne convient : échec (« aucun bloc de maillage exploitable »).
4. **Sommets.** `nv = vb / 80` sommets de 80 octets chacun, à partir de `p + 8`. Pour le sommet `k`, à la base `b = p + 8 + 80k` :
   - position : 3 × `f32` en `b + 0` (x, y, z) ;
   - coordonnées de texture : 2 × `f32` en `b + 72` (u, v) ;
   - les octets `b+12 … b+71` sont ignorés.
5. **Indices.** `ni = ib / 2` valeurs `u16`, à partir de `p + 8 + vb + 4`. Si un indice est `≥ nv` : échec global (« indice invalide »), et aucun maillage partiel n'est conservé.
6. **Primitive.** Liste de triangles (3 indices par triangle), indices sur 16 bits.
7. **Placement.** Avec `S` = (largeur, hauteur) de l'objet image en pixels, la position transmise au GPU vaut (`S.x / 2 + x`, `S.y / 2 − y`, `z`). Elle est recalculée si `S` change. Les UV sont transmises telles quelles.

Le journal de succès mentionne le chemin, la version (`MDLV0021` ou `MDLV0023`), le nombre de sommets et le nombre d'indices.

Informatif : la structure réelle d'un fichier MDLV contient, entre le magic et le bloc de sommets, plusieurs `u32` et une chaîne de chemin de matériau. Elle est suivie d'un bloc de squelette `MDLS…` puis d'un bloc d'animation `MDLA…`. Les blocs `MDLS` et `MDLA` ne sont **pas** exploités : pas de skinning ni d'animation osseuse.

### 3.2 Formats délégués à des décodeurs tiers

Ces fichiers sont lus par le système de fichiers virtuel (§4) puis transmis à un décodeur générique, sans analyse propre :

| Usage | Accès | Décodage |
|---|---|---|
| Pochette de l'élément média en cours (`$mediaThumbnail`, §4.5) | flux | décodeur d'images du §2.6 (même ensemble de formats), sortie forcée en RGBA8. Vecteur de résolution (l, h, l, h), aucun drapeau. Une image de largeur ou hauteur nulle est ignorée. |
| Polices des objets texte | lecture complète en mémoire | moteur de polices (TTF/OTF…). Les noms qui commencent par `systemfont_` désignent une police système et ne sont pas lus. Un échec bascule sur la police système. |
| Sons des objets son | flux | décodeur audio générique (MP3, OGG, WAV…). |
| Fond d'écran de type vidéo | **chemin physique** (§4.9) | lecteur vidéo externe. Une vidéo stockée **dans** un `.pkg` ne peut donc pas être lue (QO-18). |
| Fond d'écran de type web | flux, via un schéma d'URL personnalisé | navigateur embarqué ; le chemin de l'URL, sans son `/` initial, est le chemin demandé (spec web). |
| Vidéo embarquée dans un `.tex` | mémoire | lecteur vidéo (§2.10.2). |

---

## 4. Système de fichiers virtuel et résolution des assets

### 4.1 Concepts

Chaque fond d'écran chargé possède **sa propre** instance de système de fichiers virtuel (SFV). Un SFV est une **liste ordonnée de points de montage**. Chaque point de montage associe :

- une **racine**, c'est-à-dire un préfixe de chemin virtuel (`/` dans presque tous les cas) ;
- un **adaptateur**, qui expose trois opérations : `existe(chemin)`, `ouvrir(chemin)` qui renvoie un flux, et `chemin_physique(chemin)`.

#### 4.1.1 Types d'adaptateurs

| Adaptateur | Source | `existe` | `ouvrir` | `chemin_physique` |
|---|---|---|---|---|
| Mémoire | table chemin → contenu | §4.4 | §4.4 | toujours `Introuvable` |
| Archive | fichier `.pkg` (§1) | §1.5 | §1.6 | toujours `Introuvable` |
| Répertoire | dossier réel | §4.2 | §4.2 | §4.2 |
| Pochette média | lecteur multimédia | §4.5 | §4.5 | toujours `Invalide` |

#### 4.1.2 Choix de l'adaptateur au montage

Monter le chemin source `P` à la racine `R` : on teste les fabriques d'adaptateurs **dans l'ordre suivant**, et la première qui accepte `P` crée l'adaptateur.

1. **Mémoire** : accepte si la chaîne `P` vaut exactement `virtual` (QO-19).
2. **Archive** : accepte si `P`, après résolution complète des liens symboliques, existe, est un fichier régulier **et** a l'extension exacte `.pkg` (sensible à la casse, extension évaluée sur le chemin résolu). Toute erreur pendant ce test vaut refus.
3. **Répertoire** : accepte si `P`, après résolution des liens, existe et est un dossier. La racine de l'adaptateur est le chemin **canonique** (absolu, sans liens symboliques).
4. **Pochette média** : accepte si `P` vaut exactement `$mediaThumbnail`.

Si aucune fabrique n'accepte : erreur `Introuvable` (« aucun adaptateur ne peut monter ce chemin »). Si la fabrique choisie échoue à la création (par exemple `E_PKG_ENTETE`), l'erreur est propagée et les fabriques suivantes ne sont pas essayées.

Un point de montage ajouté se place **en fin de liste**.

### 4.2 Adaptateur répertoire

Notation : `B` = racine canonique de l'adaptateur, `r` = chemin demandé.

- `existe(r)` : on canonicalise `B/r` en résolvant les liens. Si la canonicalisation échoue (cible absente, par exemple), on renvoie « faux ». On renvoie « vrai » si et seulement si la chaîne du chemin canonique **commence par** la chaîne `B` (comparaison de **préfixe de chaîne**, pas de composants : QO-20) **et** que la cible est un fichier régulier. Un dossier donne donc « faux ».
- `ouvrir(r)` : mêmes contrôles. On lève `Introuvable` si le préfixe ne correspond pas ou si la cible est absente, et `Introuvable` (« dossier au lieu d'un fichier ») si la cible n'est pas un fichier régulier. Sinon on ouvre le fichier en lecture. Lumen DOIT l'ouvrir en **mode binaire** (QO-21).
- `chemin_physique(r)` : renvoie le chemin canonique s'il respecte le préfixe, sinon `Introuvable`. Il n'y a **pas** de test « fichier régulier » ici.
- Conséquence : un lien symbolique placé dans le dossier et pointant hors de `B` est invisible.

### 4.3 Adaptateur archive

- `existe(r)` : `r` correspond-il au nom d'une entrée (§1.5) ?
- `ouvrir(r)` : copie l'entrée en mémoire (§1.6). Une entrée absente donne `Introuvable`.
- `chemin_physique(r)` : `Introuvable` (« non pris en charge »).

### 4.4 Adaptateur mémoire

- C'est une table qui associe un chemin (comparé composant par composant) à un contenu en octets.
- `existe(r)` : la clé `r` est-elle présente ?
- `ouvrir(r)` : renvoie le flux associé à `r`. **Dans l'amont, c'est le même objet flux à chaque ouverture, sans retour au début.** Une seconde lecture du même fichier virtuel renvoie donc un contenu vide (QO-22). Une clé absente donne `Introuvable`.
- L'ajout d'une entrée existante la remplace.

#### 4.4.1 Contenu pré-enregistré

Tout SFV de fond d'écran contient d'office un adaptateur mémoire monté à `/` en **première** position. Il expose les cinq fichiers suivants (clés sans `/` initial). Le sens de ces fichiers relève des specs de rendu ; leur contenu est décrit ici pour que le SFV soit complet.

1. **`effects/wpenginelinux/bloomeffect.json`** : document d'effet JSON avec :
   - `name` = `"camerabloom_wpengine_linux"`, `group` = `"wpengine_linux_camera"`, `dependencies` = tableau vide ;
   - `passes` = tableau de 4 objets, dans l'ordre :

   | # | `material` | `target` | `bind` (liste d'objets `{name, index}`) |
   |---|---|---|---|
   | 1 | `materials/util/downsample_quarter_bloom.json` | `_rt_4FrameBuffer` | `_rt_FullFrameBuffer` → 0 |
   | 2 | `materials/util/downsample_eighth_blur_v.json` | `_rt_8FrameBuffer` | `_rt_4FrameBuffer` → 0 |
   | 3 | `materials/util/blur_h_bloom.json` | `_rt_Bloom` | `_rt_8FrameBuffer` → 0 |
   | 4 | `materials/util/combine.json` | `_rt_FullFrameBuffer` | `_rt_imageLayerComposite_-1_a` → 0 ; `_rt_Bloom` → 1 |

2. **`models/wpenginelinux.json`** : objet JSON `{ "material": "materials/wpenginelinux.json" }`.
3. **`materials/wpenginelinux.json`** : objet JSON avec `passes` = tableau d'**un** objet : `blending` = `"normal"`, `cullmode` = `"nocull"`, `depthtest` = `"disabled"`, `depthwrite` = `"disabled"`, `shader` = `"genericimage2"`, `textures` = `["_rt_FullFrameBuffer"]`.
4. **`shaders/commands/copy.vert`** : shader de sommets GLSL minimal. Entrées `a_Position` (vec3) et `a_TexCoord` (vec2), sortie `v_TexCoord` (vec2). La position de sortie vaut `a_Position` complétée par w = 1, et `v_TexCoord` reçoit `a_TexCoord`.
5. **`shaders/commands/copy.frag`** : shader de fragments GLSL minimal. Uniform `sampler2D g_Texture0`, entrée `v_TexCoord`. La couleur de sortie est l'échantillon de `g_Texture0` en `v_TexCoord`, écrit dans la variable de sortie conventionnelle du pipeline de traduction de shaders (nommée `out_FragColor` dans l'amont ; voir la spec shaders).

Les JSON sont sérialisés de façon compacte. L'ordre des clés n'a pas d'importance.

### 4.5 Adaptateur pochette média

- Monté avec la source **et** la racine `$mediaThumbnail`.
- `existe(r)` : « vrai » si et seulement si `r` est la chaîne vide. Avec la règle du §4.7, seul le chemin demandé exact `$mediaThumbnail` aboutit ici.
- `ouvrir(chemin)` reçoit le chemin normalisé complet (§4.7), qui DOIT valoir `$mediaThumbnail`, sinon `Introuvable`. Ensuite :
  1. on lit l'URL de pochette de l'élément en cours de lecture (métadonnée MPRIS `mpris:artUrl`, spec média). Si elle est absente : `Introuvable` ;
  2. l'URL DOIT commencer par `file://`, sinon `Introuvable` (les URL `http(s)://` sont refusées). On retire les 7 premiers caractères **sans aucun décodage pourcent** (QO-23) ;
  3. le résultat est rendu absolu, relativement au dossier courant ;
  4. le chemin doit exister (sinon `Introuvable`) et être un fichier régulier (sinon `Introuvable`) ;
  5. on ouvre le fichier en lecture binaire.
- `chemin_physique` : erreur de catégorie `Invalide` (et non `Introuvable` : QO-24).

### 4.6 Normalisation lexicale des chemins

Toute requête est d'abord normalisée **lexicalement**, sans accès disque :

1. Séparateur : `/`. Sur la plateforme d'origine, `\` est un caractère ordinaire (QO-25 pour Windows).
2. Une suite de séparateurs est réduite à un seul.
3. Les composants `.` sont supprimés.
4. Un composant `x` (différent de `..`) suivi de `..` est supprimé avec ce `..`. On répète jusqu'à stabilité.
5. Si le chemin est absolu (commence par `/`), les `..` placés juste après la racine sont supprimés : `/../a` devient `/a`.
6. Les `..` en tête d'un chemin relatif sont conservés : `../a` reste `../a`.
7. Un chemin devenu vide s'écrit `.`. Par exemple `a/..` devient `.`.
8. Un séparateur final est conservé : `a/b/` reste `a/b/`, et `a/./` devient `a/`.

Exemples : `./project.json` → `project.json` ; `materials//x/../y.tex` → `materials/y.tex`.

### 4.7 Algorithme de résolution

Pour une requête `q` :

1. `n` = normalisation de `q`.
2. On parcourt les points de montage **dans l'ordre de la liste** :
   - si la chaîne `n` ne commence pas par la chaîne de la racine `R`, on passe au suivant ;
   - sinon on prend `rel` = `n` privé de ses `|R|` premiers caractères. Si `adaptateur.existe(rel)`, ce point de montage est **retenu** et la recherche s'arrête.
3. Si aucun point n'est retenu et que `n` ne commence pas par `/` : on recommence **une** fois à l'étape 1 avec la requête `"/" + n`. La normalisation de cette nouvelle requête retire les `..` de tête (règle 5).
4. Sinon : `Introuvable` (« fichier absent de tous les points de montage »).

**Point subtil, à reproduire :** l'opération finale (`ouvrir` ou `chemin_physique`) reçoit le chemin normalisé **de la requête d'origine** (`n` de l'étape 1 du premier appel), et **non** `rel`. Cas pratiques :

- requête relative ordinaire (`materials/x.tex`) : `n = rel`, aucune différence ;
- requête absolue (`/materials/x.tex`) : `existe` réussit sur `materials/x.tex`, mais `ouvrir` reçoit `/materials/x.tex`. L'archive et la mémoire échouent alors (`Introuvable`). Le répertoire échoue aussi, car `B` joint à un chemin absolu donne le chemin absolu lui-même, qui ne respecte pas le préfixe `B` ;
- requête `../x` : `existe` est évalué sur `x` (après la reprise de l'étape 3), mais `ouvrir` reçoit `../x` et échoue.

Les requêtes absolues ou commençant par `..` aboutissent donc à `Introuvable` à l'ouverture, alors que la résolution réussit (QO-26).

### 4.8 Ordre de montage d'un fond d'écran

Soit `W` le chemin du fond d'écran (§5.1) et `A` le dossier d'assets (§5.4). L'ordre des points de montage, qui est aussi l'**ordre de priorité**, est :

| Priorité | Source | Racine | Si le montage échoue |
|---|---|---|---|
| 1 | adaptateur mémoire pré-enregistré (§4.4.1) | `/` | (toujours présent) |
| 2 | `$mediaThumbnail` (§4.5) | `$mediaThumbnail` | (toujours présent) |
| 3 | `W` (dossier, ou `.pkg` donné directement) | `/` | erreur **propagée** : le fond d'écran ne se charge pas |
| 4 | `W/scene.pkg` | `/` | absent ou invalide : ignoré. S'il est invalide, l'erreur est journalisée. |
| 5 | `W/gifscene.pkg` | `/` | idem |
| 6 | `A` | `/` | **erreur fatale** : « aucun dossier d'assets valide », avec le chemin résolu |
| 7 | dossier de travail courant du processus | `/` | ignoré |

Conséquences à reproduire :

- un fichier présent **à la fois** en clair dans `W` et dans `scene.pkg` est lu **depuis le dossier** : le dossier a priorité sur l'archive ;
- un fichier absent du fond d'écran est cherché dans les assets de l'installation, puis dans le dossier courant (QO-27) ;
- le premier fichier lu est `project.json`.

### 4.9 Opérations de haut niveau

| Opération | Comportement |
|---|---|
| Lire un flux | résolution (§4.7) puis `ouvrir`. Si le flux renvoyé est dans un état d'échec, on lève une erreur `Invalide` (« échec d'ouverture »), et non `Introuvable`. |
| Lire une chaîne | lit le flux en entier et renvoie les octets bruts, sans retrait de BOM ni conversion. |
| Chemin physique | résolution (§4.7) puis `chemin_physique`. Seul l'adaptateur répertoire réussit. |

À la frontière du localisateur d'assets, les erreurs `Introuvable` restent `Introuvable`. Les autres (`Invalide`) sont propagées sans conversion. Ce point compte pour les replis des §4.10 et §4.11.

### 4.10 Conventions de chemins par type d'asset

| Asset | Chemin demandé au SFV |
|---|---|
| Descripteur de projet | `project.json` |
| Fichier principal (scène, vidéo, page web) | valeur de la clé `file` de `project.json` (vidéo : via **chemin physique**) |
| Texture de nom logique `N` | `materials/` + `N` + `.tex`. On **ajoute** `.tex` à `N`, qui est conservé tel quel, sous-dossiers et extension compris : `N = foo.png` donne `materials/foo.png.tex`. |
| Métadonnées de planche | `materials/` + `N` + `.tex-json` |
| Shader de sommets / fragments / inclusion de nom `S` | `shaders/` + `S` dont la **dernière extension est remplacée** par `.vert` / `.frag` / `.h`, ou ajoutée s'il n'y en a pas. Exemple : `effects/blur` donne `shaders/effects/blur.frag`. Attention, `a/b.c` donne `a/b.frag` (QO-28). |
| Shader « workshop » | voir §4.10.1 |
| Matériaux, effets, modèles, particules | chemin tel que référencé dans le JSON, sans préfixe |
| Maillage de marionnette | valeur `puppet` du modèle (§3.1) |
| Polices, sons | chemin tel que référencé |
| Pochette média | `$mediaThumbnail` |

#### 4.10.1 Substitution de compatibilité des shaders workshop

On l'applique au nom de shader `S'`, déjà muni de sa nouvelle extension et **relatif** à `shaders/` :

1. Si le premier composant de `S'` est `workshop`, que le deuxième composant (`id`) existe **et** que `S'` a au moins **4** composants :
   - on prend `f` = **4ᵉ** composant, et lui seul (les suivants sont ignorés) ;
   - on tente de lire `zcompat/scene/shaders/<id>/<f>`. En cas de succès, on renvoie ce contenu et on journalise le remplacement (nom d'origine → nom de compatibilité) ;
   - si cette lecture échoue avec `Introuvable`, on ignore l'échec et on passe à l'étape 2.
2. On lit `shaders/` + `S'`. Un échec produit l'erreur de chargement d'asset habituelle.

Exemples : `workshop/123/effects/foo.frag` essaie d'abord `zcompat/scene/shaders/123/foo.frag`. En revanche `workshop/123/foo.frag`, qui n'a que 3 composants, ne déclenche **aucune** tentative de compatibilité.

### 4.11 Cache de textures et recherche multi-fonds

- Les textures sont mises en cache **par nom logique**, pour tout le contexte de rendu. Ce cache est partagé entre tous les fonds d'écran chargés, quel que soit l'écran (QO-29).
- Noms réservés, qui ne correspondent à aucun fichier :
  - `$mediaThumbnail` : pochette en cours ;
  - `$mediaPreviousThumbnail` : pochette précédente ;
  - noms qui commencent par `_rt_` ou `_alias_` : surfaces de rendu de la scène (spec rendu).
- Quand un nom est absent du cache, on interroge le SFV de **chaque fond d'écran chargé** dans l'ordre lexicographique, octet par octet, de leurs clés. Une clé vaut `default`, un nom d'écran ou `span:<premier écran du groupe>`, et toutes les clés sont triées ensemble :
  - si l'ouverture de `materials/N.tex` échoue avec `Introuvable`, on essaie le fond suivant ;
  - si elle réussit, on analyse le fichier (§2), on lit les métadonnées (§2.9.2), on crée la texture GPU (§2.10), on la met en cache et on la renvoie ;
  - une erreur `Invalide` (analyse, LZ4, format GPU…) **n'est pas** rattrapée : elle est propagée.
- Si aucun fond ne fournit la texture : `Introuvable` (« fichier introuvable : N »).
- La pochette média est cherchée de la même façon : premier SFV qui fournit `$mediaThumbnail` et dont l'image se décode avec une taille non nulle.

---

## 5. Localisation Steam et dossier d'assets

L'amont ne prend en charge que Linux. Tout ce qui suit décrit ce comportement Linux. Pour Windows, voir QO-30.

### 5.1 Interprétation de l'argument « fond d'écran »

Pour l'argument positionnel du fond par défaut et pour les fonds par écran :

- s'il ne contient **aucun** caractère `/`, c'est un **identifiant workshop**, résolu par le §5.2. L'identifiant n'est pas vérifié comme numérique ;
- sinon c'est un chemin, utilisé tel quel (relatif au dossier courant s'il n'est pas absolu).

Conséquence : `monfond` est traité comme un identifiant workshop. Il faut écrire `./monfond` pour désigner un dossier local.

### 5.2 Dossier workshop

On utilise l'identifiant d'application Steam de Wallpaper Engine, **431960**, et le dossier personnel `H` (§5.5). On teste dans l'ordre les candidats suivants et on retient le premier qui **existe et est un dossier** :

1. `H/.local/share/Steam/steamapps/workshop/content/431960/<id>`
2. `H/.steam/steam/steamapps/workshop/content/431960/<id>`
3. `H/.var/app/com.valvesoftware.Steam/.local/share/Steam/steamapps/workshop/content/431960/<id>`
4. `H/snap/steam/common/.local/share/Steam/steamapps/workshop/content/431960/<id>`

Si aucun candidat ne convient : erreur fatale « dossier workshop introuvable pour l'application 431960 et le contenu `<id>` ».

### 5.3 Dossier d'installation

Nom du dossier d'application : `wallpaper_engine`. Pour un sous-chemin `s`, on teste dans l'ordre les candidats suivants. **L'ordre des deux premiers est inversé par rapport au §5.2.** On retient le premier qui existe et est un dossier :

1. `H/.steam/steam/steamapps/common/wallpaper_engine/<s>`
2. `H/.local/share/Steam/steamapps/common/wallpaper_engine/<s>`
3. `H/.var/app/com.valvesoftware.Steam/.local/share/Steam/steamapps/common/wallpaper_engine/<s>`
4. `H/snap/steam/common/.local/share/Steam/steamapps/common/wallpaper_engine/<s>`

Si aucun candidat ne convient : erreur « dossier introuvable pour l'application `wallpaper_engine` : `<s>` ».

Utilisations :

- `s = assets` : dossier d'assets (§5.4) ;
- `s` vide : dossier d'installation, dans lequel on lit `config.json`, utilisé uniquement pour les listes de lecture (spec CLI/config). En cas d'échec, l'erreur est « installation introuvable pour lire config.json ».

### 5.4 Dossier d'assets

On l'établit une fois au démarrage, par ordre de priorité :

1. l'option `--assets-dir <chemin>`, si elle est non vide. Le chemin est utilisé **tel quel**, sans vérification à ce stade, et un message signale qu'il vient de l'option ;
2. sinon le dossier d'installation avec `s = assets` (§5.3) ;
3. sinon, si l'étape 2 échoue, le dossier `assets` situé à côté de l'exécutable : `dossier_parent(chemin_canonique_de_l_exécutable)/assets`. Il n'est pas vérifié à ce stade.

La vérification effective a lieu au montage (§4.8, priorité 6). Si le chemin retenu n'est ni un dossier ni un `.pkg`, l'erreur est fatale.

Contenu attendu (informatif) : l'arborescence `assets` de l'installation de Wallpaper Engine (`materials/`, `shaders/`, `effects/`, `models/`, `particles/`, `fonts/`, `zcompat/`…).

### 5.5 Dossier personnel

`H` = variable d'environnement `HOME`. Si elle n'est pas définie : erreur « dossier personnel introuvable ». Si elle ne désigne pas un dossier existant : erreur « `<HOME>` n'est pas un dossier ». `H` n'est consulté que lorsqu'une recherche Steam est nécessaire : `--assets-dir` fourni avec un fond donné par chemin n'en a pas besoin.

### 5.6 Nettoyage des chemins issus de `config.json`

Les éléments de liste de lecture lus dans `config.json` sont des chemins Windows. Ils sont convertis ainsi :

1. une chaîne vide reste vide ;
2. on retire le préfixe littéral `\\?\` s'il est présent ;
3. on remplace chaque `\` par `/` ;
4. si le 2ᵉ caractère est `:` (lettre de lecteur), on retire les 2 premiers caractères ;
5. si le résultat est non vide et ne commence pas par `/`, on préfixe `/` ;
6. on applique la normalisation lexicale (§4.6) ;
7. si le résultat désigne un **fichier** régulier existant, on remplace le chemin par son dossier parent.

---

## 6. Modèle de données recommandé (non normatif)

| Objet | Champs |
|---|---|
| Archive | chemin source ; liste ordonnée des entrées (nom en octets, offset `u32`, taille `u32`) ; `base` ; accès en lecture positionnelle |
| Texture analysée | version effective du conteneur (1–4) ; version d'animation (0 = aucune, 1–3) ; format (`u32`) ; drapeaux (`u32`) ; `LT`, `HT`, `LI`, `HI` ; `LA`, `HA` ; `TIE` (`i32`) ; `est_mp4` ; `NI` ; images (liste de listes de mipmaps) ; frames ; `colonnes`, `lignes`, `frames_planche`, `durée_planche` |
| Mipmap | largeur, hauteur (de l'en-tête) ; compression ; taille finale ; octets décompressés ; métadonnées texte (v4 effective) |
| Frame | F0 `u32` ; F1 à F7 `f32` |
| Maillage de marionnette | positions brutes (x, y, z) ; UV ; indices `u16` |

Dérivés utiles : « animée » = (drapeaux & 4) ≠ 0 ; « a une planche » = `frames_planche > 0`.

---

## 7. Fixtures de test

Toutes les valeurs sont en hexadécimal, little-endian. Les tables donnent l'offset du premier octet de chaque champ. Chaque fixture a été produite par un petit générateur écrit d'après cette spec et vérifiée par relecture. La PNG de F6 a été validée par un décodeur tiers.

Recette générale pour fabriquer un fichier à la main :

- `u32` v : 4 octets, poids faible en premier (1 → `01 00 00 00`, 0xFFFFFFFF → `FF FF FF FF`) ;
- `f32` : 0.5 → `00 00 00 3F`, 1.0 → `00 00 80 3F`, −1.0 → `00 00 80 BF`, 0.0 → `00 00 00 00` ;
- `sstr` « abc » : `03 00 00 00 61 62 63` ;
- `magic9` `TEXV0005` : `54 45 58 56 30 30 30 35 00`.

### F1 — `.pkg` minimal (35 octets) : un fichier `a.txt` contenant `hi`

```
0000: 08 00 00 00 50 4B 47 56 30 30 30 31 01 00 00 00
0010: 05 00 00 00 61 2E 74 78 74 00 00 00 00 02 00 00
0020: 00 68 69
```

| Offset | Octets | Champ |
|---|---|---|
| 0x00 | `08 00 00 00` | longueur de l'en-tête = 8 |
| 0x04 | `50 4B 47 56 30 30 30 31` | « PKGV0001 » |
| 0x0C | `01 00 00 00` | N = 1 |
| 0x10 | `05 00 00 00` | longueur du nom = 5 |
| 0x14 | `61 2E 74 78 74` | « a.txt » |
| 0x19 | `00 00 00 00` | offset = 0 |
| 0x1D | `02 00 00 00` | taille = 2 |
| 0x21 | `68 69` | données (`base` = 0x21 = 33) |

Résultat attendu : `base = 33`. `ouvrir("a.txt")` renvoie `68 69`. `existe("A.txt")` est faux (casse). `existe("b")` est faux.

### F2 — `.pkg` à deux entrées et offset non nul (77 octets)

```
0000: 08 00 00 00 50 4B 47 56 30 30 31 39 02 00 00 00
0010: 0C 00 00 00 70 72 6F 6A 65 63 74 2E 6A 73 6F 6E
0020: 00 00 00 00 07 00 00 00 0F 00 00 00 6D 61 74 65
0030: 72 69 61 6C 73 2F 78 2E 74 65 78 07 00 00 00 03
0040: 00 00 00 7B 22 61 22 3A 31 7D 58 59 5A
```

| Offset | Champ | Valeur |
|---|---|---|
| 0x00 | en-tête | « PKGV0019 » |
| 0x0C | N | 2 |
| 0x10 | entrée 1 : nom | « project.json » (12 octets) |
| 0x20 | entrée 1 : offset / taille | 0 / 7 |
| 0x28 | entrée 2 : nom | « materials/x.tex » (15 octets) |
| 0x3B | entrée 2 : offset / taille | 7 / 3 |
| 0x43 | données | `{"a":1}` puis `XYZ` |

Résultat attendu : `base = 67`. `project.json` occupe [67, 74) et `materials/x.tex` occupe [74, 77). La requête `./materials//x.tex` se normalise en `materials/x.tex` et renvoie `XYZ`. La requête `/materials/x.tex` est trouvée par la résolution mais **échoue** à l'ouverture (§4.7).

Variantes négatives : remplacer l'octet 0x04 (`50`) par `51` (« QKGV… ») donne `E_PKG_ENTETE`. Remplacer l'en-tête par `04 00 00 00 50 4B 47 56` (« PKGV » seul) est **accepté**.

### F3 — `.tex` TEXB0003, RGBA 2×2 non compressé (103 octets)

```
0000: 54 45 58 56 30 30 30 35 00 54 45 58 49 30 30 30
0010: 31 00 00 00 00 00 00 00 00 00 02 00 00 00 02 00
0020: 00 00 02 00 00 00 02 00 00 00 00 00 00 00 54 45
0030: 58 42 30 30 30 33 00 01 00 00 00 FF FF FF FF 01
0040: 00 00 00 02 00 00 00 02 00 00 00 00 00 00 00 10
0050: 00 00 00 10 00 00 00 FF 00 00 FF 00 FF 00 FF 00
0060: 00 FF FF FF FF FF FF
```

| Offset | Champ | Valeur |
|---|---|---|
| 0x00 | magic | TEXV0005\0 |
| 0x09 | magic | TEXI0001\0 |
| 0x12 | format | 0 (RGBA8888) |
| 0x16 | drapeaux | 0 |
| 0x1A / 0x1E | `LT` / `HT` | 2 / 2 |
| 0x22 / 0x26 | `LI` / `HI` | 2 / 2 |
| 0x2A | réservé | 0 |
| 0x2E | magic | TEXB0003\0 |
| 0x37 | `NI` | 1 |
| 0x3B | `TIE` | −1 (inconnu) |
| 0x3F | `NM` (image 0) | 1 |
| 0x43 / 0x47 | largeur / hauteur mip 0 | 2 / 2 |
| 0x4B | compression | 0 |
| 0x4F | `TD` | 16 |
| 0x53 | `TS` | 16 |
| 0x57 | pixels | rouge, vert, bleu, blanc (RGBA) |

Résultat attendu : 1 image, 1 mip, taille finale 16, texture GPU RGBA8, adressage en répétition, filtrage trilinéaire, résolution (2, 2, 2, 2), zone UV (1, 1), non animée, pas de planche.

### F4 — `.tex` TEXB0003, RGBA 1×1 compressé LZ4 (92 octets)

```
0000: 54 45 58 56 30 30 30 35 00 54 45 58 49 30 30 30
0010: 31 00 00 00 00 00 02 00 00 00 01 00 00 00 01 00
0020: 00 00 01 00 00 00 01 00 00 00 00 00 00 00 54 45
0030: 58 42 30 30 30 33 00 01 00 00 00 FF FF FF FF 01
0040: 00 00 00 01 00 00 00 01 00 00 00 01 00 00 00 04
0050: 00 00 00 05 00 00 00 40 11 22 33 44
```

Points notables : drapeaux = 2 (`UV_BORNES`) à 0x16, compression = 1 à 0x4B, `TD` = 4 à 0x4F, `TS` = 5 à 0x53. Le bloc LZ4 de 5 octets est `40 11 22 33 44` : un jeton `0x40` (4 littéraux, aucune copie) suivi des 4 littéraux. C'est la plus petite séquence finale valide du format bloc LZ4.

Résultat attendu : pixel RGBA = (0x11, 0x22, 0x33, 0x44), adressage clamp to edge.

Recette pour un bloc LZ4 sans compression effective, à utiliser pour des données plus grandes : pour n < 15 littéraux, jeton = `n << 4` suivi des n octets. Pour n ≥ 15, jeton = `F0`, puis des octets d'extension dont la somme vaut `n − 15` (chaque `FF` ajoute 255 et le dernier octet est < `FF`), puis les n octets. Exemple pour 16 octets : `F0 01` + 16 octets.

Variantes négatives :

- jeton `50` (5 littéraux annoncés) avec `TS` = 5 et 4 littéraux disponibles donne `E_TEX_LZ4` ;
- compression = 2 avec `TD` = 4 : l'analyse lit 4 octets bruts (`40 11 22 33`), ce qui décale la suite du flux (comportement QO-08).

### F5 — `.tex` TEXB0001, R8, texture 4×1 dont l'image utile fait 3×1 (79 octets)

```
0000: 54 45 58 56 30 30 30 35 00 54 45 58 49 30 30 30
0010: 31 00 09 00 00 00 01 00 00 00 04 00 00 00 01 00
0020: 00 00 03 00 00 00 01 00 00 00 00 00 00 00 54 45
0030: 58 42 30 30 30 31 00 01 00 00 00 01 00 00 00 04
0040: 00 00 00 01 00 00 00 04 00 00 00 00 55 AA FF
```

Points notables : format 9 (R8), drapeaux 1 (`SANS_INTERPOLATION`), `LT` = 4 et `LI` = 3. En v1 il n'y a pas de `TIE` : `NM` est à 0x3B. La mip 0 a pour largeur 4 (0x3F), hauteur 1 (0x43), `TS` 4 (0x47), et ses données sont à 0x4B.

Résultat attendu : texture R8 4×1 de valeurs 0, 85, 170, 255. Filtrage au plus proche. Résolution (4, 1, 3, 1). Zone UV u ∈ [0, 0.75]. Combo `TEX0FORMAT` = 9.

### F6 — `.tex` TEXB0003 avec PNG embarquée 1×1 (157 octets)

La PNG embarquée fait 70 octets et contient un pixel RGBA (0x11, 0x22, 0x33, 0xFF) :

```
89 50 4E 47 0D 0A 1A 0A 00 00 00 0D 49 48 44 52
00 00 00 01 00 00 00 01 08 06 00 00 00 1F 15 C4
89 00 00 00 0D 49 44 41 54 78 DA 63 10 54 32 FE
0F 00 02 14 01 66 DB BE CC 6F 00 00 00 00 49 45
4E 44 AE 42 60 82
```

Le fichier `.tex` complet se construit ainsi : en-tête identique à F4 (format 0, drapeaux 2, toutes les dimensions à 1), puis `TEXB0003\0`, `NI` = 1, **`TIE` = 13** (`0D 00 00 00`, PNG), `NM` = 1, mip largeur 1, hauteur 1, compression 0, `TD` = 70 (`46 00 00 00`), `TS` = 70, puis les 70 octets de la PNG à partir de 0x57.

```
0000: 54 45 58 56 30 30 30 35 00 54 45 58 49 30 30 30
0010: 31 00 00 00 00 00 02 00 00 00 01 00 00 00 01 00
0020: 00 00 01 00 00 00 01 00 00 00 00 00 00 00 54 45
0030: 58 42 30 30 30 33 00 01 00 00 00 0D 00 00 00 01
0040: 00 00 00 01 00 00 00 01 00 00 00 00 00 00 00 46
0050: 00 00 00 46 00 00 00 89 50 4E 47 0D 0A 1A 0A 00
0060: 00 00 0D 49 48 44 52 00 00 00 01 00 00 00 01 08
0070: 06 00 00 00 1F 15 C4 89 00 00 00 0D 49 44 41 54
0080: 78 DA 63 10 54 32 FE 0F 00 02 14 01 66 DB BE CC
0090: 6F 00 00 00 00 49 45 4E 44 AE 42 60 82
```

Résultat attendu : texture GPU RGBA8 1×1 = (0x11, 0x22, 0x33, 0xFF), résolution (1, 1, 1, 1).

Variantes :

- `TIE` = 2 (JPEG) avec la même PNG : **même résultat**, puisque le format est reniflé ;
- `TIE` = 37 donne `E_TEX_TIE` ;
- remplacer la PNG par 70 octets aléatoires : texture 1×1 noire transparente et avertissement (règle Lumen).

### F7 — `.tex` animé : 2 images 1×1 et TEXS0003 à 2 frames (204 octets)

```
0000: 54 45 58 56 30 30 30 35 00 54 45 58 49 30 30 30
0010: 31 00 00 00 00 00 06 00 00 00 01 00 00 00 01 00
0020: 00 00 01 00 00 00 01 00 00 00 00 00 00 00 54 45
0030: 58 42 30 30 30 33 00 02 00 00 00 FF FF FF FF 01
0040: 00 00 00 01 00 00 00 01 00 00 00 00 00 00 00 04
0050: 00 00 00 04 00 00 00 FF 00 00 FF 01 00 00 00 01
0060: 00 00 00 01 00 00 00 00 00 00 00 04 00 00 00 04
0070: 00 00 00 00 00 FF FF 54 45 58 53 30 30 30 33 00
0080: 02 00 00 00 01 00 00 00 01 00 00 00 00 00 00 00
0090: 00 00 00 3F 00 00 00 00 00 00 00 00 00 00 80 3F
00A0: 00 00 00 00 00 00 00 00 00 00 80 3F 01 00 00 00
00B0: 00 00 00 3F 00 00 00 00 00 00 00 00 00 00 80 3F
00C0: 00 00 00 00 00 00 00 00 00 00 80 3F
```

| Offset | Champ | Valeur |
|---|---|---|
| 0x16 | drapeaux | 6 (`ANIME` + `UV_BORNES`) |
| 0x37 | `NI` | 2 |
| 0x3F | image 0 | `NM` = 1 ; mip 1×1, C = 0, `TD` = `TS` = 4 ; pixel rouge opaque |
| 0x5B | image 1 | idem ; pixel bleu opaque |
| 0x77 | magic | TEXS0003\0 |
| 0x80 | `NF` | 2 |
| 0x84 / 0x88 | `LA` / `HA` | 1 / 1 |
| 0x8C | frame 0 | F0 = 0, F1 = 0.5, F2 = F3 = 0, F4 = 1, F5 = F6 = 0, F7 = 1 |
| 0xAC | frame 1 | F0 = 1, sinon identique |

Résultats attendus :

- texture animée ; taille réelle 1×1 ; résolution (1, 1, 1, 1) ;
- planche : `col = lig = 1` et 1 × 1 < 2, donc **aucune planche** (toutes les valeurs à 0) ;
- `T = 1.0` ; on lie l'image 0 (rouge) à t = 0.25, l'image 0 à t = 0.5 (égalité), l'image 1 (bleu) à t = 0.75, et l'image 0 à t = 1.0 (le `fmod` donne 0) ;
- `g_Texture0Translation` = (0, 0) et `g_Texture0Rotation` = (1, 0, 0, 1) ;
- variante : `LI` = 2 (0x22 = `02`) et F4 = 1 donnent `col = 2`, `lig = 1`, 2 ≥ 2, donc une planche de 2 colonnes, 1 ligne, 2 frames et durée 1.0 ;
- variante : magic `TEXS0004\0` donne `E_TEX_TEXS`.

### F8 — `.tex` TEXB0004 vidéo MP4 (110 octets)

```
0000: 54 45 58 56 30 30 30 35 00 54 45 58 49 30 30 30
0010: 31 00 00 00 00 00 20 00 00 00 40 00 00 00 40 00
0020: 00 00 40 00 00 00 40 00 00 00 00 00 00 00 54 45
0030: 58 42 30 30 30 34 00 01 00 00 00 FF FF FF FF 01
0040: 00 00 00 01 00 00 00 01 00 00 00 00 00 00 00 7B
0050: 7D 00 00 00 00 00 40 00 00 00 40 00 00 00 00 00
0060: 00 00 04 00 00 00 04 00 00 00 4D 50 34 21
```

| Offset | Champ | Valeur |
|---|---|---|
| 0x16 | drapeaux | 0x20 (`VIDEO`) |
| 0x1A…0x26 | dimensions | 64 partout |
| 0x2E | magic | TEXB0004\0 |
| 0x37 | `NI` | 1 |
| 0x3B | `TIE` | −1 |
| 0x3F | indicateur MP4 | 1, donc `TIE` passe à 35 et la version effective reste 4 |
| 0x43 | `NM` | 1 |
| 0x47 / 0x4B | réservés A / B | 1 / 0 |
| 0x4F | métadonnées | `cstr` « {} » (3 octets avec le `00`) |
| 0x52 | réservé C | 0 |
| 0x56 / 0x5A | largeur / hauteur | 64 / 64 |
| 0x5E / 0x62 / 0x66 | C / `TD` / `TS` | 0 / 4 / 4 |
| 0x6A | données | « MP4! » (fichier vidéo factice) |

Résultat attendu de l'analyse : `est_mp4` vrai, `TIE` = 35, version effective 4, une mipmap de 4 octets, métadonnées « {} ». À la création GPU, on passe par le chemin vidéo (la lecture de ce contenu factice échoue côté lecteur, ce qui sort du périmètre de la spec).

Variante : indicateur MP4 = 0 et `TIE` = −1. La version effective redescend à 3 et la mip se lit **sans** les champs réservés : la lecture commence à 0x47 avec largeur = 1, hauteur = 0, etc. Ce test vérifie la règle de rétrogradation : le flux est alors désaligné et la lecture doit se terminer par `FinDeFluxInattendue`.

### F9 — maillage de marionnette `MDLV0021` (276 octets)

Construction :

| Offset | Taille | Contenu |
|---|---|---|
| 0 | 9 | `4D 44 4C 56 30 30 32 31 00` (« MDLV0021\0 ») |
| 9 | 4 | `00 00 00 00` (ignoré) |
| 13 | 4 | `vb` = 240 = `F0 00 00 00` (3 sommets × 80) |
| 17 | 240 | 3 sommets de 80 octets, nuls sauf position en +0 (3 × `f32`) et UV en +72 (2 × `f32`) : s0 = (−1, 1, 0 ; 0, 0), s1 = (1, 1, 0 ; 1, 0), s2 = (−1, −1, 0 ; 0, 1) |
| 257 | 4 | `ib` = 6 = `06 00 00 00` |
| 261 | 6 | indices `00 00 01 00 02 00` |
| 267 | 9 | « MDLS0001\0 » |

Les 40 premiers octets :

```
0000: 4D 44 4C 56 30 30 32 31 00 00 00 00 00 F0 00 00
0010: 00 00 00 80 BF 00 00 80 3F 00 00 00 00 00 00 00
0020: 00 00 00 00 00 00 00 00
```

Résultats attendus : `L` = 267 ; bloc trouvé à p = 9 ; 3 sommets ; 3 indices ; avec une image de taille S = (100, 50), les positions GPU valent (49, 24, 0), (51, 24, 0) et (49, 26, 0).

Variantes :

- magic `MDLV0014` : échec non fatal, quadrilatère standard ;
- indice `03 00` : échec « indice invalide » ;
- supprimer `MDLS0001\0` : `L` = taille du fichier = 267, et le résultat est identique ;
- `vb` = 239 : aucun bloc trouvé à p = 9, la recherche continue aux positions suivantes et, sur ce fichier, finit en échec.

### F10 — cas négatifs `.tex` (partir de F3 et modifier un octet)

| Modification | Attendu |
|---|---|
| octet 0x08 : `00` → `20` | `E_TEX_TEXV` |
| octet 0x0C : `49` → `4A` (« TEXJ ») | `E_TEX_TEXI` |
| format (0x12) = `03 00 00 00` | `E_TEX_FORMAT` |
| format = `01 00 00 00` (RGB888) | analyse réussie, puis `E_TEX_FORMAT_GPU` à la création |
| drapeaux (0x16) = `2F 00 08 00` (524335) | `E_TEX_DRAPEAUX` |
| drapeaux = `10 00 00 00` (16, bit non documenté) | accepté |
| drapeaux = `08 00 00 00` (`UV_BORNES_BORDURE`) | accepté ; adressage en **répétition** |
| octet 0x35 : `33` → `35` (« TEXB0005 ») | `E_TEX_TEXB` |
| tronquer le fichier à 100 octets | `FinDeFluxInattendue` |
| `TS` (0x53) = `FF FF FF FF` (−1) | `E_TEX_TAILLE` |

---

## 8. Questions ouvertes

Chaque entrée décrit le comportement amont observé puis une recommandation pour Lumen, à confirmer.

- **QO-01 — Lectures au-delà de la fin.** L'amont ne contrôle jamais l'état du flux. Une archive ou une texture tronquée produit des valeurs indéterminées, avec un risque de blocage sur une `cstr` sans terminateur. *Reco :* erreur `FinDeFluxInattendue` (§0.3).
- **QO-02 — Bornes du `.pkg`.** Aucune vérification des longueurs de `sstr`, des offsets ni des tailles. Les offsets et `base` sont sur 32 bits, ce qui limite les archives à 4 Gio. *Reco :* contrôles du §1.4 ; ils ne changent rien pour les archives valides.
- **QO-03 — Version du `.pkg`.** Seul le préfixe `PKGV` est vérifié et aucune différence de disposition entre versions n'est connue. *À confirmer* sur un corpus réel que toutes les versions partagent la disposition du §1.2.
- **QO-04 — Validation des drapeaux.** Le test « < 524335 » refuse la combinaison de tous les bits connus et accepte des bits inconnus. Cela ressemble à une erreur de borne (≤ visé ?). *Reco :* reproduire tel quel par fidélité, et journaliser les bits inconnus.
- **QO-05 — `UV_BORNES_BORDURE` ignoré** pour les textures `.tex`, mais honoré pour les surfaces de rendu dérivées. Probable oubli. *Reco :* reproduire, puis réévaluer d'après des captures du logiciel original.
- **QO-06 — Ambiguïté 35 = WEBP = MP4.** Dans un `TEXB0004`, `TIE = 35` sans indicateur MP4 garde le format de mip v4 et tente un décodage d'image qui échoue, faute de décodeur WEBP dans l'amont. *Reco :* reproduire la disposition. Si Lumen sait décoder WEBP, il peut l'afficher (divergence visible à arbitrer).
- **QO-07 — Ensemble de décodeurs d'images.** L'amont décode par reniflage un ensemble limité de formats (§2.6), sans WEBP ni TIFF. En cas d'échec, le contenu est indéterminé. *Reco :* même ensemble au minimum, zéros en cas d'échec. Décider si les formats supplémentaires sont activés.
- **QO-08 — Compression inconnue et taille LZ4.** Avec `C` ≥ 2, on lit `TD` octets bruts au lieu de `TS`. Le nombre d'octets décompressés n'est pas comparé à `TD`. *Reco :* reproduire la lecture `C ≥ 2` et le remplissage par zéros ; ajouter un avertissement.
- **QO-09 — Image sans mipmap (`NM = 0`).** Plage de niveaux de mip négative (débordement) et accès hors bornes si la taille de texture de cette image est demandée. *Reco :* traiter comme une image 1×1 transparente et journaliser.
- **QO-10 — Alignement des lignes.** L'amont fixe l'alignement de dépaquetage à 1 octet uniquement lors de l'envoi d'une texture R8, **et ne le restaure jamais**. Une texture RG88 de largeur impaire est donc lue différemment selon qu'une R8 a été chargée avant ou non. *Reco :* toujours considérer les données comme strictement contiguës (alignement 1).
- **QO-11 — TEXS sans frame.** Avec `NF = 0` en TEXS0001/0002, l'amont lit une frame inexistante. *Reco :* `LA = HA = 0`, pas de planche ; l'animation affiche l'image 0.
- **QO-12 — Format GPU validé avant le test vidéo.** Une texture qui porte le drapeau `VIDEO`, sans image embarquée et avec un format de pixels non pris en charge, est refusée. *Reco :* reproduire, cas rare.
- **QO-13 — Champ réservé à 0x2A et champs réservés de la mip v4.** Leur sens est inconnu. Les métadonnées JSON de la mip v4 sont ignorées. *À documenter* si un corpus révèle un usage.
- **QO-14 — Valeurs de frame TEXS0001.** F5 et F6 sont ignorés (valent 0), alors qu'ils contiennent probablement `wy` et `hx` en entiers. Les entiers sont convertis en flottant tels quels (non signés). *Reco :* reproduire.
- **QO-15 — Analyse heuristique MDLV.** L'amont cherche un motif (pas de 80 octets par sommet, indices multiples de 6) au lieu de suivre la structure. Il ignore squelette et animations. La documentation amont décrit un sommet de 52 octets alors que le code en utilise 80. La recherche de `MDLS` ne teste pas la toute dernière position possible (`p + 4 = |D|`). *Reco :* reproduire l'heuristique à l'identique. Une analyse structurée viendra dans une spec dédiée si besoin.
- **QO-16 — Versions MDLV.** Seules `MDLV0021` et `MDLV0023` sont acceptées. *À confirmer :* existe-t-il d'autres versions dans le corpus ?
- **QO-17 — Conversion flottant → entier** pour `LA` et `HA` en TEXS0001/0002 : une valeur négative ou NaN produit un comportement indéfini. *Reco :* saturer à 0.
- **QO-18 — Vidéos dans une archive.** Un fond d'écran vidéo exige un chemin physique, qui n'existe pas pour une entrée de `.pkg`. *Reco :* reproduire l'échec, ou extraire vers un fichier temporaire (divergence à arbitrer).
- **QO-19 — Nom réservé `virtual`.** Monter un chemin source dont la chaîne vaut exactement `virtual` crée un adaptateur mémoire vide au lieu de monter le dossier `./virtual`. *Reco :* reproduire, effet de bord quasi inatteignable (le chemin du fond d'écran contient normalement un `/`).
- **QO-20 — Contrôle de confinement par préfixe de chaîne.** Avec `B = /a/b`, un lien symbolique vers `/a/bc/x` est accepté. *Reco :* comparer par composants (divergence de sécurité mineure, invisible sur des données normales).
- **QO-21 — Ouverture en mode texte.** L'adaptateur répertoire et l'adaptateur pochette ouvrent les fichiers sans le drapeau binaire. C'est neutre sous Linux, mais une reproduction naïve sous Windows corromprait les binaires (CRLF). *Reco :* toujours en binaire.
- **QO-22 — Flux mémoire non rembobiné.** La deuxième lecture d'un même fichier virtuel renvoie un contenu vide (par exemple deux passes utilisant `shaders/commands/copy.frag`). *Reco :* renvoyer un nouveau curseur à chaque ouverture. Cela diverge d'un bogue latent : vérifier qu'aucun rendu ne dépend de ce vide.
- **QO-23 — URL de pochette.** Seul `file://` est accepté et les séquences `%xx` ne sont pas décodées : un chemin avec espaces (`%20`) échoue. *Reco :* reproduire, ou décoder (amélioration à arbitrer).
- **QO-24 — Catégorie d'erreur incohérente.** Le chemin physique sur l'adaptateur pochette lève une erreur générique et non `Introuvable`. *Reco :* sans impact pratique, utiliser `Introuvable`.
- **QO-25 — Séparateurs sous Windows.** L'amont n'est pas porté sous Windows. *Reco :* sous Windows, avant la normalisation, convertir `\` en `/` dans les chemins **demandés** (issus des JSON), comparer les entrées de `.pkg` octet à octet après cette conversion, et utiliser le séparateur natif uniquement pour l'accès disque. La comparaison reste sensible à la casse dans les `.pkg`. Elle ne l'est pas sur un disque NTFS : faut-il forcer une vérification de casse pour reproduire Linux ?
- **QO-26 — Requêtes absolues ou en `..`.** Elles sont résolues mais échouent à l'ouverture (§4.7). *Reco :* reproduire, car c'est le comportement observable (fichier introuvable). Une alternative serait de transmettre `rel` à l'adaptateur, ce qui rendrait accessibles des fichiers introuvables dans l'amont.
- **QO-27 — Dossier courant monté en dernier recours.** Des fichiers du dossier de lancement peuvent masquer des assets manquants. *Reco :* reproduire, éventuellement derrière une option de diagnostic.
- **QO-28 — Remplacement d'extension des shaders.** Un nom de shader contenant un point perd sa dernière « extension », et le chemin de compatibilité workshop n'utilise que le 4ᵉ composant. Un chemin `workshop/<id>` à 2 composants provoque un comportement indéfini dans l'amont. *Reco :* reproduire ; pour 2 composants, ne pas tenter la compatibilité.
- **QO-29 — Cache de textures global par nom.** Deux fonds d'écran affichés simultanément qui contiennent chacun une texture de même nom logique mais de contenu différent partagent la première chargée. *Reco :* reproduire par fidélité ; signaler comme limitation connue.
- **QO-30 — Windows : Steam et assets.** L'amont ne définit rien pour Windows : il dépend de `HOME` et de `/proc/self/exe`. *Proposition à valider (hors amont) :* lire l'emplacement de Steam dans le registre (`HKCU\Software\Valve\Steam`, valeur `SteamPath`), avec `C:\Program Files (x86)\Steam` comme valeur par défaut, et parcourir les bibliothèques déclarées dans `steamapps/libraryfolders.vdf`. Ordre : bibliothèque principale, puis bibliothèques additionnelles, dans l'ordre du fichier. Le dossier de l'exécutable sert de repli pour `assets`. La règle « pas de `/` donc identifiant workshop » (§5.1) doit aussi accepter `\` comme séparateur, sinon `C:\x\y` serait pris pour un identifiant.
- **QO-31 — Erreurs non rattrapées.** Une texture corrompue (catégorie `Invalide`) fait échouer tout le rendu au lieu de passer au fond suivant ou d'afficher une texture de substitution. Un commentaire amont évoque une texture à damier. *Reco :* reproduire ; envisager plus tard un damier derrière une option.
- **QO-32 — `.pkg` présent mais invalide.** Il est ignoré silencieusement, avec seulement une ligne de journal. Le fond d'écran peut alors se charger partiellement depuis les assets globaux. *Reco :* reproduire.
- **QO-33 — Documentation amont divergente.** Le document de format `.tex` publié avec l'amont présente le `u32` qui suit le magic TEXB comme « inconnu », alors que le code l'utilise comme nombre d'images, et il omet la boucle par image. Cette spec suit le comportement effectif.
- **QO-34 — Hôtes gros-boutistes.** L'amont lirait les `u32` dans l'ordre de l'hôte sur une machine gros-boutiste. Cette situation est hors cible, et Lumen lit toujours en little-endian.
