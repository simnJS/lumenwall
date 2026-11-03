# Spec 03 — Shaders : dialecte Wallpaper Engine, prétraitement, traduction, uniforms

Statut : rédigé en salle blanche à partir de l'observation du moteur de référence (révision amont `a0ddce5`, front-end GLSL patché `b775500a`, back-end patché `ad4d0222`). Aucun code amont n'est reproduit ; seuls figurent des faits imposés par le format Wallpaper Engine (noms d'uniforms, de macros, de clés JSON, valeurs d'énumérations, formules).

Mots normatifs : **DOIT**, **NE DOIT PAS**, **DEVRAIT**, **PEUT**. Les comportements marqués *« amont »* décrivent ce que fait la référence, défauts compris ; quand un comportement semble être un bug, il est signalé et reporté dans les Questions ouvertes (§17).

Renvois : les autres specs couvrent le système de fichiers virtuel, le format `.tex`, les objets image/effets/particules (enchaînement des passes, FBO, géométrie), les propriétés utilisateur / valeurs dynamiques, la caméra et l'audio. Ce document n'en reprend que ce qui est nécessaire pour comprendre les shaders.

---

## 0. Vocabulaire

| Terme | Sens |
|---|---|
| **Pass** | Un dessin : un shader (paire vertex + fragment), un jeu de textures, d'uniforms et d'états. Il provient d'une entrée `passes[]` d'un fichier matériau (`materials/*.json`). |
| **Unité** | Le texte source d'une étape (vertex ou fragment) d'un shader. |
| **Surcharge de pass** | L'entrée `passes[i]` d'une instance d'effet dans `scene.json` (`objects[].effects[].passes[]`). Elle peut fournir `combos`, `constantshadervalues`, `textures`, `usertextures`. Le moteur peut aussi en fabriquer (§5.4). |
| **Combo** | Macro de préprocesseur entière (`#define NOM valeur`) qui sélectionne des variantes de code. |
| **Combos explicites** | Combos du pass matériau + combos imposés au pass + combos de la surcharge. |
| **Combos découverts** | Combos déduits de l'analyse des sources (déclarations `[COMBO]`, samplers liés à un combo). |
| **Emplacement (slot)** | Index entier ≥ 0 d'une texture d'un pass ; l'emplacement N correspond au sampler `g_TextureN`. |
| **Clé matériau** | Valeur de la clé `"material"` d'une annotation d'uniform ; c'est sous ce nom que les JSON matériau/scène désignent l'uniform. |

---

## 1. Vue d'ensemble (par pass)

1. Déterminer le nom du shader : celui de la surcharge s'il en impose un (§5.4), sinon la clé `"shader"` du pass matériau.
2. Construire les combos explicites du pass (§5.1).
3. Lire `shaders/<nom>.vert` et `shaders/<nom>.frag` (§2).
4. Pour chaque unité, dans cet ordre : includes (§4.1), modules `#require` (§4.2), analyse ligne à ligne des combos et annotations (§4.3), remplacement de `gl_FragColor` (§4.4).
5. Assembler la source finale de chaque unité : en-tête, macros de compatibilité, défines de combos, corps, adaptations de varyings (§9).
6. Valider et traduire les deux unités ensemble avec un front-end GLSL **permissif** (§10), puis émettre du GLSL 330 core.
7. Créer le programme GPU. Toute erreur ⇒ l'objet entier (toutes ses passes) est abandonné et journalisé ; les autres objets continuent (§10.4).
8. Enregistrer les uniforms : annotations → constantes du matériau → constantes de la surcharge → textures → uniforms moteur → uniforms propres au type d'objet (§12.1).
9. À chaque image : lier les textures (§8.4), téléverser les uniforms (§12.4), dessiner.

---

## 2. Fichiers sources et localisation

### 2.1 Noms de fichiers

- Le nom de shader est un chemin relatif sans extension, séparateur `/` (ex. `genericimage2`, `effects/waterwaves`, `workshop/123456/effects/foo`).
- Unité vertex : `shaders/<nom>` avec l'extension remplacée par `vert`. Unité fragment : idem avec `frag`.
- Include `#include "X"` : `shaders/X` avec l'extension remplacée par `h` (donc `common` → `shaders/common.h`, `foo.inc` → `shaders/foo.h`). Le chemin est toujours relatif à la racine `shaders/`, jamais au dossier du fichier qui inclut.
- « Remplacer l'extension » : si le dernier composant du chemin contient un point (hors point initial), ce qui suit le dernier point est remplacé ; sinon l'extension est ajoutée. Conséquence : un nom de shader `a/b.c` donne `shaders/a/b.vert`.
- Aucune autre étape n'est chargée : les fichiers `.geom` (et tout autre suffixe) sont ignorés. Le macro `GS_ENABLED` n'est jamais défini, donc les blocs `#if GS_ENABLED` sont inactifs.
- Les fichiers sont lus en texte brut (octets), sans conversion d'encodage ni de fins de ligne.

### 2.2 Ordre de recherche

Les shaders et includes sont lus dans le système de fichiers virtuel du fond d'écran. Premier trouvé, premier servi, dans cet ordre :

1. Fichiers virtuels fournis par le moteur (§2.4).
2. Pseudo-fichier de pochette média (sans objet pour les shaders).
3. Dossier du fond d'écran.
4. `scene.pkg` du dossier, s'il existe.
5. `gifscene.pkg` du dossier, s'il existe.
6. Dossier `assets` de Wallpaper Engine (contient les shaders de base et leurs includes : `common.h`, `common_blending.h`, `common_particles.h`, etc.).
7. Répertoire courant du processus.

Un fond d'écran peut donc remplacer un shader ou un include de base en fournissant le même chemin.

### 2.3 Substitution de compatibilité « workshop »

Avant la recherche normale, si le chemin demandé (shader **ou** include, après ajout de l'extension) a la forme `workshop/<id>/<c3>/<c4>[/…]` (premier composant exactement `workshop`, **au moins quatre composants**), le moteur essaie d'abord `zcompat/scene/shaders/<id>/<c4>`, où `<c4>` est le **quatrième** composant (les composants suivants sont ignorés, `<c3>` aussi). S'il existe, son contenu est utilisé à la place (message de journal). Sinon, recherche normale de `shaders/<chemin>`. Avec seulement trois composants, aucune substitution n'est tentée. Les fichiers `zcompat/…` sont livrés dans le dossier `assets` de Wallpaper Engine.

### 2.4 Shader virtuel `commands/copy`

Le moteur fournit en mémoire `shaders/commands/copy.vert` et `shaders/commands/copy.frag`, utilisés par les passes d'effet de type commande `"command": "copy"` (voir spec effets). Comportement :

- vertex : entrées `a_Position` (vec3) et `a_TexCoord` (vec2) ; position de sortie = `(a_Position, 1)` **sans aucune matrice** ; transmet `a_TexCoord` dans la varying `v_TexCoord` ;
- fragment : couleur = échantillon de `g_Texture0` en `v_TexCoord`.

Ces sources sont déjà en syntaxe GLSL moderne (`in`/`out`) et passent par exactement le même pipeline que les autres. Le pass virtuel associé a : blending `normal`, culling désactivé, depth test et depth write désactivés, texture d'emplacement 0 = la `source` de la commande, cible = la `target` de la commande, aucun combo, aucune constante.

---

## 3. Le dialecte Wallpaper Engine

### 3.1 Base

Les sources sont du GLSL « ancien style » enrichi :

- pas de ligne `#version` (le moteur l'ajoute ; une source qui en contient une produit une erreur de compilation) ;
- vertex : entrées déclarées `attribute`, sorties `varying` ; fragment : entrées `varying`, sortie écrite dans `gl_FragColor` ;
- échantillonnage via `texSample2D(sampler, uv)` et `texSample2DLod(sampler, uv, lod)` ;
- fonctions et types de style HLSL (§3.3) ;
- uniforms toujours déclarés hors bloc (`uniform <type> <nom>;`), éventuellement annotés (§6) ;
- directives propres : `#include "…"`, `#require <Module>`, déclarations de combos dans des commentaires `// [COMBO] {…}` ;
- branches `#if GLSL` / `#if HLSL` : seul `GLSL` est défini (à 1).

Le langage accepté après prétraitement est plus permissif que le GLSL standard (conversions implicites, surcharges lâches…) : voir §10.2, **à reproduire**.

### 3.2 Exemple (illustratif, rédigé pour cette spec)

```glsl
// [COMBO] {"material":"ui_editor_properties_blend_mode","combo":"BLENDMODE","type":"imageblending","default":0}
#include "common.h"

uniform sampler2D g_Texture0; // {"hidden":true}
uniform sampler2D g_Texture1; // {"combo":"MASK","default":"util/white","label":"Masque"}
uniform float g_Strength; // {"material":"strength","default":1,"range":[0,2]}
uniform vec3 g_Tint; // {"material":"tint","default":"1 1 1","type":"color"}

varying vec4 v_TexCoord;

void main() {
    vec4 albedo = texSample2D(g_Texture0, v_TexCoord.xy);
#if MASK
    albedo.a *= texSample2D(g_Texture1, v_TexCoord.zw).r;
#endif
    gl_FragColor = vec4(lerp(albedo.rgb, albedo.rgb * g_Tint, saturate(g_Strength)), albedo.a);
}
```

### 3.3 Macros de compatibilité prédéfinies

Le moteur définit, avant toute ligne de la source, les identifiants suivants (macros de préprocesseur textuelles, sémantique C : remplacement d'arguments, pas de récursion). Ce sont les seuls ajouts ; tout autre identifiant HLSL (`rsqrt`, `tex2D`, `float3x3`, `half`, `clip`, …) n'est pas traduit.

| Forme dans le dialecte | Équivalent | Remarque |
|---|---|---|
| `mul(a, b)` | `(b) * (a)` | Ordre inversé : convention vecteur-ligne HLSL ; les matrices sont téléversées en colonne-majeure non transposées. |
| `max(a, b)` | fonction native `max` avec arguments **échangés** : `max(b, a)` | Résultat identique ; permet `max(0.0, v)` avec `v` vectoriel. `min` n'est pas modifié. |
| `lerp` | `mix` | |
| `frac` | `fract` | |
| `saturate(x)` | `clamp(x, 0.0, 1.0)` | |
| `CAST2(x)`, `CAST3(x)`, `CAST4(x)` | `vec2(x)`, `vec3(x)`, `vec4(x)` | parenthésé |
| `CAST3X3(x)` | `mat3(x)` | parenthésé |
| `float2`, `float3`, `float4` | `vec2`, `vec3`, `vec4` | |
| `int2`, `int3`, `int4` | `ivec2`, `ivec3`, `ivec4` | |
| `texSample2D` | `texture` | |
| `texSample2DLod` | `textureLod` | |
| `log10(x)` | `log2(x) * 0.301029995663981` | constante = log10(2) |
| `atan2` | `atan` (forme à deux arguments, même ordre y, x) | |
| `fmod(x, y)` | `x - y * trunc(x / y)` | sémantique HLSL (signe de x) |
| `ddx` | `dFdx` | |
| `ddy(x)` | `dFdy(-(x))`, c.-à-d. `-dFdy(x)` | Inversion de signe : émule l'axe vertical « vers le bas » de D3D sur OpenGL. Voir §16. |
| `GLSL` | `1` | |

En outre : `precision highp float;` est émis (sans effet en GLSL desktop).

Par unité :

- vertex : `attribute` ≡ `in`, `varying` ≡ `out` ;
- fragment : `varying` ≡ `in`, et une sortie `vec4 out_FragColor` est déclarée (emplacement 0 implicite).

### 3.4 Attributs de sommets fournis par le moteur

Le moteur lie les attributs **par nom** ; un attribut absent du programme est ignoré.

| Contexte | Attributs (type, composantes fournies) |
|---|---|
| Images, effets, commandes, maillages « puppet » | `a_Position` (3 floats), `a_TexCoord` (2 floats) ; 6 sommets en triangles (ou maillage indexé 16 bits pour un puppet). |
| Particules, rendu « sprite » (17 floats/sommet) | `a_Position` (3), `a_TexCoordVec4` (4), `a_Color` (4), `a_TexCoordVec4C1` (4), `a_TexCoordC2` (2) — dans cet ordre dans le tampon entrelacé. |
| Particules, rendu « corde » (26 floats/sommet) | `a_PositionVec4` (4), `a_TexCoordVec4` (4), `a_TexCoordVec4C1` (4), `a_TexCoordVec4C2` (4), `a_TexCoordVec4C3` (4), `a_TexCoordC4` (2), `a_Color` (4). |

Si le shader déclare un attribut plus large que ce qui est fourni, les composantes manquantes valent (0, 0, 0, 1) (règle standard des attributs de sommets).

---

## 4. Prétraitement textuel d'une unité (ordre exact)

Le prétraitement opère sur du texte brut, par recherche de sous-chaînes, **sans tenir compte des commentaires ni des branches conditionnelles** (sauf mention contraire). Ces approximations font partie du comportement observable.

### 4.1 Includes

#### 4.1.1 Collecte (fichier principal)

Chaque occurrence de la sous-chaîne `#include` dans le texte de l'unité — y compris dans un commentaire ou une branche `#if` inactive — est traitée, dans l'ordre du texte :

1. Le nom est le texte compris entre le premier `"` qui suit la directive et le `"` suivant.
2. La directive est neutralisée en remplaçant ses deux premiers caractères `#i` par `//` (la ligne devient un commentaire, la longueur du texte ne change pas).
3. Le contenu du fichier (§2.1, §2.2, §2.3) est ajouté à la fin d'un **bloc d'includes**, encadré par deux lignes de commentaire marqueurs (début/fin). Si le fichier est introuvable, une seule ligne de commentaire signalant l'absence est ajoutée à la place : ce n'est **pas** une erreur.

Il n'y a ni déduplication (un même fichier inclus deux fois est copié deux fois) ni prise en compte des conditions : un include placé dans `#if X … #endif` est inclus inconditionnellement.

#### 4.1.2 Expansion des includes imbriqués

Dans le bloc d'includes, chaque ligne contenant `#include` est remplacée, du début de la directive jusqu'à la fin de la ligne (fin de texte si pas de saut de ligne), par le contenu du fichier référencé (mêmes marqueurs, même traitement de l'absence). L'analyse reprend au début du contenu inséré : l'expansion est donc récursive, en profondeur d'abord, **sur place** (contrairement aux includes du fichier principal qui sont regroupés). Aucune détection de cycle *(amont : un cycle boucle indéfiniment ; Lumen DOIT détecter un cycle et le traiter comme une erreur de l'unité)*.

#### 4.1.3 Point d'insertion du bloc

Le bloc d'includes est inséré **une seule fois**, en début de ligne, à une position P déterminée ainsi :

1. **Localiser `main`** : première occurrence de la séquence « espace, `main` » immédiatement suivie d'un espace ou d'une `(`. Les occurrences suivies d'autre chose (`mainColor`…) sont sautées. Aucune occurrence ⇒ échec de l'unité (exception, objet abandonné). Attention : `void\tmain` ou `main` en tout début de ligne ne sont pas reconnus ; une occurrence dans un commentaire l'est.
2. **Candidat** : soit K la dernière occurrence, **avant** la position de `main`, de l'une des sous-chaînes `attribute`, `varying` ou `uniform` (recherche brute, commentaires et identifiants compris). Si K existe, P = début de la ligne qui suit la ligne de K. Sinon P = début de la ligne de `main`.
3. **Sortie des blocs conditionnels** : on apparie, sur tout le texte, chaque `#if` (ce qui couvre `#ifdef`/`#ifndef`) avec le `#endif` correspondant (pile ; `#else`/`#elif` ignorés ; `#endif` orphelin ignoré). Les paires sont examinées dans l'ordre de leurs `#endif`. Si P est strictement après le `#if` et au plus au `#endif`, P est ramené au début de la ligne du `#if`. Comme les blocs englobants se ferment plus tard, P remonte jusqu'au bloc le plus externe qui le contient (cas typique : un fichier contenant deux `main` dans `#if GS_ENABLED … #else … #endif`).
4. **Borne** : P ne peut pas être après le début de la ligne de `main` (on prend le minimum des deux).
5. Le bloc d'includes, suivi d'un saut de ligne, est inséré en P. Si `main` est sur la première ligne, P = début du texte.

*Défaut amont* : si le `#if` à remonter est sur la toute première ligne du texte, l'amont insère après le premier caractère. Lumen DEVRAIT insérer en début de texte (§17).

Conséquence voulue : le code des includes voit les uniforms/varyings/attributs déclarés par le fichier principal avant `main`, et les fonctions du fichier principal déclarées après ces déclarations voient le code des includes.

### 4.2 Modules `#require`

Sur le texte obtenu (includes insérés), chaque occurrence de `#require` (même dans un commentaire) est traitée :

- Le nom de module est le reste de la ligne après `#require ` (le mot suivi d'un espace), espaces et `\r` finaux retirés ; les espaces initiaux supplémentaires sont conservés (le nom ne correspond alors à aucun module).
- Nom vide, ou directive sans espace suivie directement de la fin de ligne : erreur journalisée, la directive est **laissée intacte** (elle provoquera ensuite une erreur de compilation).
- Sinon la directive est neutralisée (`#r` → `//`) et :
  - module `LightingV1` : une définition de fonction est insérée juste avant la directive : `vec3 PerformLighting_V1(vec3, vec3, vec3, vec3, vec3, vec3, float, float)` — dans l'ordre : position monde, albédo, normale, direction de vue, teinte spéculaire, réflectance de base, rugosité, métallicité — qui renvoie toujours `vec3(0.0)` (aucune lumière dynamique : les objets lumière ne sont pas gérés) ;
  - tout autre module : erreur journalisée, rien n'est inséré.

### 4.3 Analyse ligne à ligne : combos et annotations

Le texte résultant (fichier principal + includes + modules) est parcouru ligne par ligne. Seules les lignes terminées par `\n` sont examinées (la dernière ligne sans saut de ligne final est ignorée). Pour chaque ligne :

1. Si elle contient la sous-chaîne `// [COMBO] ` (avec l'espace final), le texte qui suit ce marqueur est un objet JSON de déclaration de combo (§5.2). La ligne n'est pas examinée pour une annotation.
2. Sinon, si elle contient `uniform ` (avec espace), `// ` et `;`, **et** que le premier `;` précède le premier `// ` (exclut les lignes entièrement commentées ; les commentaires `/* */` ne sont pas gérés), c'est une déclaration annotée :
   - nom = caractères entre le dernier espace situé avant le premier `;` et ce `;` ;
   - type = caractères entre l'espace précédent et ce dernier espace ;
   - JSON = tout ce qui suit le premier `//`.
   - Seuls des espaces (pas de tabulations) séparent les mots. `uniform float g_X ;` donne un nom vide et un « type » `g_X` (ignoré, §6.4). `uniform float g_X[4];` donne le nom `g_X[4]`, qui ne correspondra à aucun uniform (ignoré). Un qualificatif supplémentaire (`uniform highp float g_X;`) est toléré.
3. JSON invalide (y compris texte parasite après l'objet) : erreur journalisée, ligne ignorée.

### 4.4 `gl_FragColor`

Enfin, toute occurrence de la sous-chaîne `gl_FragColor` est remplacée par `out_FragColor`, dans les deux unités, partout (identifiants plus longs compris).

---

## 5. Combos

### 5.1 Sources et priorités

Du plus prioritaire au moins prioritaire :

1. **Combos de la surcharge de pass** : `combos` de l'entrée de `scene.json` (objet `{"NOM": entier}`), ou combos imposés par le moteur (§5.4).
2. **Combos du pass matériau** : `combos` de l'entrée `passes[]` du matériau, auxquels le moteur ajoute/écrase `TEX0FORMAT` (§5.4).
3. **Combos découverts** dans l'unité elle-même (§5.2, §5.3).
4. **Combos découverts dans l'autre unité** du même shader.

Les valeurs JSON de combos sont converties en entier ; un booléen donne 1/0, un nombre flottant est tronqué ; une chaîne est une erreur de chargement. Les combos ne sont **pas** liés aux propriétés utilisateur : leur valeur est figée au chargement. Les propriétés utilisateur influencent les combos seulement indirectement (visibilité d'un effet ⇒ présence de ses passes).

Correspondance des noms : les recherches (« ce combo est-il déjà fourni ? ») sont **sensibles à la casse et exactes** ; les `#define` émis sont en **majuscules** (conversion ASCII). Deux noms qui ne diffèrent que par la casse produisent donc un seul `#define` : le premier émis gagne (§5.5).

### 5.2 Déclarations `[COMBO]`

Clés JSON :

| Clé | Rôle |
|---|---|
| `combo` | **Obligatoire.** Nom du combo. Absente ⇒ échec de l'unité (exception, objet abandonné). |
| `default` | Valeur par défaut entière. |
| `type`, `label`, `options`, `material`, `require`, … | Ignorées (éditeur). |

Règle : si le nom n'est présent ni dans les combos du pass ni dans ceux de la surcharge (correspondance exacte), il est ajouté aux combos découverts avec :

- `default` absent ⇒ 0 ;
- `default` entier (signé ou non) ⇒ cette valeur ;
- `default` flottant, chaîne, booléen ou autre ⇒ échec de l'unité (exception, objet abandonné).

Si le même combo est déclaré plusieurs fois dans l'unité, la **première** déclaration rencontrée l'emporte. Un combo déclaré est donc **toujours** défini (valeur 0 par défaut), ce qui le distingue des combos de textures (§5.3) qui peuvent rester non définis.

### 5.3 Combos liés aux textures (annotations de samplers)

Pour un uniform de type `sampler2D` ou `sampler2DComparison` dont l'annotation porte une clé `combo` :

**Définitions.** L'emplacement S est le chiffre qui suit `g_Texture` dans le nom (§6.4). S est **utilisé** s'il figure dans les textures du pass matériau (`textures` ∪ `usertextures`) ou dans les `textures` de la surcharge (les `usertextures` de la surcharge, les liaisons `bind` et les textures par défaut ne comptent pas).

**Décision** (valeur V initialisée à 1) :

1. S utilisé ⇒ le combo est requis, V = 1.
2. Sinon, si l'annotation a une clé `require` (objet `{"COMBO": valeur, …}`) :
   - avec `"requireany": true` : requis dès qu'**au moins une** entrée de `require` n'est **pas** satisfaite par les combos du pass (combo absent des combos du pass, **ou** présent dans la surcharge, **ou** de valeur différente) ;
   - sans `requireany` (ou faux) : requis, **sauf** si au moins une entrée est satisfaite (combo présent dans les combos du pass avec exactement cette valeur ; un combo présent seulement dans la surcharge donne un comportement indéfini en amont).
   *(Ces deux règles paraissent inversées par rapport à l'intention probable — §17.)*
3. Sinon : non requis.
4. Si requis alors que S n'est pas utilisé : si l'annotation n'a pas de `default`, ou si le combo figure déjà dans les combos du pass ou de la surcharge ⇒ finalement non requis ; sinon V = conversion entière du `default` (nombre ⇒ sa valeur ; chaîne ⇒ lecture d'un entier décimal en tête de chaîne). *Amont : un `default` non numérique comme `"util/white"` fait échouer l'objet (§17).*
5. Si finalement requis ⇒ ajout aux combos découverts (si absent) avec V.

Sinon le combo n'est **pas défini du tout** : `#if MASK` vaut alors 0 et `#ifdef MASK` est faux.

### 5.4 Combos imposés par le moteur

| Combo | Valeur | Condition | Niveau |
|---|---|---|---|
| `TEX0FORMAT` | 8 | La texture principale de l'objet (pas la texture réellement liée au pass) est au format RG88. | Combos du pass (écrase la valeur du matériau). |
| `TEX0FORMAT` | 9 | Idem, format R8. | idem |
| `THICKFORMAT` | 1 | Toujours, pour les particules. | Surcharge |
| `SPRITESHEET` | 1 | Particules dont la texture est une planche de sprites (nombre d'images > 0). | Surcharge |
| `TRAILRENDERER` | 1 | Particules en rendu « traînée ». | Surcharge |
| `BLENDMODE` | valeur de `colorBlendMode` de l'image | Image avec `colorBlendMode` > 0 : pass supplémentaire `materials/util/effectpassthrough.json`. | Surcharge |
| `BLENDMODE` | 30 | Pass de compatibilité « teinte » (`materials/effects/tint.json`) ajouté par la spec image. | Surcharge |

Particules en rendu « corde » : le nom du shader est imposé à `genericropeparticle`.

### 5.5 Émission des `#define`

Pour chaque unité, après l'en-tête (§9), les combos sont émis comme `#define NOM_EN_MAJUSCULES valeur_décimale`, en parcourant successivement :

1. les combos de la surcharge ;
2. les combos du pass (y compris `TEX0FORMAT`) ;
3. les combos découverts de l'unité ;
4. les combos du pass vus par l'autre unité (identiques au point 2) ;
5. les combos découverts de **l'autre** unité.

À l'intérieur d'un groupe, l'ordre est l'ordre lexicographique des noms (octets). Un nom (après mise en majuscules) déjà émis est sauté : **le premier émis gagne**. Conséquences :

- un combo explicite l'emporte toujours sur un combo découvert, même pour un combo de texture ;
- les deux unités voient le même ensemble de noms ; si chacune déclare `[COMBO] X` avec un défaut différent, chaque unité garde **son propre** défaut.

### 5.6 Exemple

Matériau : `"combos": {"MASK": 0}` ; surcharge : `"textures": [null, "masks/m1"]` ; fragment : `[COMBO] BLENDMODE default 0`, `g_Texture1` avec `combo: MASK`. Résultat : `MASK` = 0 (explicite, prioritaire malgré la texture), `BLENDMODE` = 0 (découvert), présent dans les deux unités.

---

## 6. Annotations d'uniforms

### 6.1 Forme

`uniform <type> <nom>; // {objet JSON}` sur une seule ligne (règles d'extraction au §4.3).

### 6.2 Clés JSON prises en compte

| Clé | Usage |
|---|---|
| `material` | Clé matériau : nom sous lequel `constantshadervalues` désigne cet uniform (§7). Sans elle, l'uniform n'est **pas enregistré** (sa valeur par défaut n'est pas appliquée : il reste à 0). Sans effet pour les samplers. |
| `default` | Valeur par défaut (§6.3) ; pour un sampler, nom de la texture par défaut (§8.2). |
| `combo`, `require`, `requireany` | Samplers seulement (§5.3). |
| Toute autre (`label`, `range`, `hidden`, `type`, `mode`, `paintdefaultcolor`, `components`, `group`, …) | Ignorée. |

### 6.3 Valeur par défaut selon le type déclaré

| Type | Forme attendue de `default` | Résultat |
|---|---|---|
| `float` | nombre | sa valeur (booléen accepté : 1/0) |
| `float` | chaîne | **entier** lu en tête de chaîne (`"0.5"` → 0, `"2.7"` → 2) *(amont, §17)* ; chaîne non numérique ⇒ échec de l'objet |
| `int` | nombre | valeur tronquée vers zéro |
| `int` | chaîne | entier lu en tête de chaîne |
| `vec2` | chaîne `"x y"` | exactement 2 composantes, sinon échec de l'objet |
| `vec3` | chaîne `"x y z"` | exactement 3 composantes, sinon échec |
| `vec4` | chaîne `"x y z w"` | au moins 4 composantes (les suivantes sont ignorées), sinon échec |
| `vec2`/`vec3`/`vec4` | nombre | échec de l'objet (la chaîne est obligatoire) |

Format des vecteurs : composantes séparées par **un seul** espace ASCII (le nombre de composantes est le nombre d'espaces + 1 ; deux espaces consécutifs faussent le compte) ; chaque composante est lue comme un flottant décimal avec tolérance (préfixe numérique, `"abc"` → 0).

Absence de `default` : si l'annotation n'a pas de clé matériau, ou si sa clé matériau ne figure pas dans les constantes de la **surcharge** du pass (celles du matériau ne comptent pas ici), échec de l'objet (exception) pour tout type sauf `sampler2D`. Si la clé figure dans la surcharge, l'amont a un comportement indéfini ; Lumen DOIT alors utiliser 0 (ou le vecteur nul) comme défaut (§17).

### 6.4 Types et cas particuliers

- Types gérés : `float`, `int`, `vec2`, `vec3`, `vec4`, `sampler2D`, `sampler2DComparison`.
- Tout autre type (`mat3`, `mat4`, `bool`, `ivec*`, `uint`, `samplerCube`, …) : erreur journalisée, annotation ignorée (uniform à 0).
- Sampler : l'emplacement est **le seul caractère** qui suit les 9 caractères `g_Texture`, interprété comme un chiffre. `g_Texture10` → emplacement 1. Un nom qui ne suit pas ce schéma donne un index absurde (amont : caractère − `'0'`) ou un échec si le nom fait moins de 10 caractères. Lumen DOIT reproduire `g_Texture0`…`g_Texture9` et DEVRAIT ignorer les autres noms (§17).
- Un `sampler2DComparison` sans `default` fait échouer l'objet dans les mêmes conditions que les types numériques (seul `sampler2D` est exempté de la règle du §6.3).

---

## 7. Liaison aux `constantshadervalues`

### 7.1 Sources

1. `constantshadervalues` du pass matériau (`materials/*.json`, `passes[i]`).
2. `constantshadervalues` de la surcharge de pass (`scene.json`) — prioritaire.
3. Valeurs injectées par le moteur sous forme de surcharge (ex. pour la passe de teinte de compatibilité : `color` et `alpha` ; pour le bloom : `bloomstrength`, `bloomthreshold` — voir spec scène).

### 7.2 Formes de valeur acceptées

Chaque entrée `"clé": valeur` est une valeur utilisateur (même grammaire que les autres réglages de scène, cf. spec propriétés) :

- nombre (entier ⇒ entier, réel ⇒ flottant), booléen ;
- chaîne : un seul mot entièrement lisible comme flottant ⇒ flottant ; sinon selon le nombre d'espaces : 1 ⇒ vec2, 2 ⇒ vec3, ≥ 3 ⇒ vec4 ; un seul mot non numérique ⇒ chaîne. Une chaîne de couleur `"r g b"` donne donc un **vec3** (pas de canal alpha implicite) ;
- objet `{"value": …, "user": "nomPropriété"}` : liée à la propriété utilisateur ; si la propriété existe, sa valeur courante **remplace immédiatement** la valeur littérale, puis chaque modification de la propriété est propagée ;
- objet `{"value": …, "user": {"name": "p", "condition": "c"}}` : quand la propriété est une chaîne, la valeur devient le booléen `p == c` (1/0) ;
- objet avec `"script"` : valeur pilotée par script (cf. spec scripts).

### 7.3 Résolution

Pour chaque entrée de constantes (matériau, puis surcharge) : on cherche, d'abord dans l'unité vertex puis dans l'unité fragment, la première annotation dont la clé matériau est **exactement** la clé de l'entrée. Si trouvée, l'uniform portant le nom de cette annotation (celle du vertex si les deux existent) est lié à la valeur de l'entrée ; sinon l'entrée est ignorée silencieusement. Si les deux unités annotent la même clé matériau avec des noms d'uniform différents, seul l'uniform du vertex reçoit la constante.

### 7.4 Conversion vers le type GLSL déclaré

Le type de téléversement est celui de **l'annotation** (pas celui de la valeur). Chaque valeur maintient en permanence ses cinq représentations ; on lit celle du type déclaré :

| Valeur source | `float` | `int` | `vec2` | `vec3` | `vec4` |
|---|---|---|---|---|---|
| flottant f | f | trunc(f) | (f, f) | (f, f, f) | (f, f, f, f) |
| entier i | i | i | (i, i) | (i, i, i) | (i, i, i, i) |
| booléen b | 1/0 | 1/0 | diffusé | diffusé | diffusé |
| vec2 (x, y) | x | trunc(x) | (x, y) | (x, y, 0) | (x, y, 0, 0) |
| vec3 (x, y, z) | x | trunc(x) | (x, y) | (x, y, z) | (x, y, z, **0**) |
| vec4 (x, y, z, w) | x | trunc(x) | (x, y) | (x, y, z) | (x, y, z, w) |
| couleur (r, g, b, a) (propriété de type couleur) | r | trunc(255·r) | (r, g) | (r, g, b) | (r, g, b, a) |
| chaîne (sans condition) / null | 0 | 0 | 0 | 0 | 0 |

`trunc` = troncature vers zéro.

### 7.5 Dynamique

Un uniform lié à une constante est relu à **chaque image** : les changements de propriétés utilisateur ou de scripts sont visibles immédiatement, sans recompilation. Un uniform annoté sans constante garde sa valeur par défaut.

---

## 8. Textures

### 8.1 Emplacements et samplers

- L'emplacement N est lié à l'unité de texture N. Les uniforms `g_Texture0` à `g_Texture7` reçoivent respectivement les unités 0 à 7. Un sampler d'un autre nom (ou `g_Texture8`, `g_Texture9`) n'est associé à aucune unité : il reste sur l'unité 0.
- Seules des textures 2D sont liées.

### 8.2 Sources d'un emplacement (pile de priorité)

À la création du pass, chaque emplacement reçoit une **pile** de candidats. Les sources sont empilées dans cet ordre (le dernier empilé est au sommet, donc prioritaire) :

1. `default` des samplers annotés de l'unité vertex (remplace la pile) ;
2. `default` des samplers annotés de l'unité fragment ;
3. `textures` du pass matériau (tableau ; l'index est l'emplacement ; `null` ou `""` saute l'emplacement mais compte dans l'index ; un élément objet `{"name": …}` est accepté) ;
4. `usertextures` du pass matériau ;
5. `textures` de la surcharge ;
6. `usertextures` de la surcharge ;
7. liaisons `bind` de la passe d'effet (`[{"name": …, "index": N}]`) — le nom `previous` empile un candidat **vide** (sens au §8.4).

Pour une image, les `textures`/`usertextures` de l'objet `instance` de `scene.json` complètent le premier pass matériau **sans écraser** les emplacements déjà définis.

Un nom qui ne peut pas être résolu (§8.3) est journalisé et **non empilé**, sauf pour un `bind` : un bind vers un FBO inexistant fait échouer l'objet.

### 8.3 Résolution d'un nom de texture

- Nom commençant par `_rt_` ou `_alias_` : cible de rendu nommée, cherchée dans l'ordre : FBO déclarés par l'effet courant (`fbos` du fichier effet), FBO propres à l'objet, FBO de la scène (§8.7). Introuvable ⇒ erreur.
- `$mediaThumbnail`, `$mediaPreviousThumbnail` : pochettes média (spec média).
- Sinon : texture `materials/<nom>.tex`, cherchée dans les systèmes de fichiers de **tous** les fonds d'écran chargés ; mise en cache par nom, de façon globale (§14). Exemples de défauts usuels du dossier `assets` : `util/white`, `util/black`, `util/noise`.

### 8.4 Choix de la texture à chaque image

Notations : **entrée** = texture d'entrée du pass (sortie du pass précédent, ou texture de l'objet pour le premier pass) ; **entrée d'effet** = entrée mémorisée au début d'une séquence de passes écrivant dans une cible nommée, vide sinon (cf. spec effets) ; **prête** = utilisable (toujours vrai sauf pour une pochette média sans contenu).

Parcours d'une pile, du sommet vers le bas, pour un emplacement :

1. candidat vide (`previous`) : prendre l'entrée d'effet si elle est prête, sinon l'entrée si elle est prête, sinon passer au candidat suivant ;
2. candidat non vide et prêt : le prendre ;
3. sinon candidat suivant.

Pile épuisée sans résultat : entrée d'effet si prête, sinon l'entrée (même non prête).

**Emplacement 0** : s'il a une pile, ce parcours détermine la texture (la pile **masque** l'entrée : un `default` sur `g_Texture0`, ou une texture d'emplacement 0 dans le matériau, remplace donc l'entrée du pass). S'il n'a pas de pile (cas usuel des passes d'effet, dont le tableau `textures` commence par `null`), c'est l'entrée. Un `bind` d'emplacement 0 crée toujours une pile (§8.2, point 7).

Tous les emplacements ayant une pile sont liés à leur unité ; l'emplacement 0 est toujours lié (si non vide). Pour une texture animée en emplacement 0, c'est l'image courante (§8.6) qui est liée ; les autres emplacements utilisent l'image 0. Après le dessin, les unités utilisées sont déliées.

### 8.5 Vecteurs de résolution `g_TextureNResolution`

Valeur : `vec4(largeur_texture, hauteur_texture, largeur_réelle, hauteur_réelle)` :

| Texture | xy | zw |
|---|---|---|
| `.tex` fixe | taille allouée (éventuellement complétée en puissance de deux ; pour une image compressée embarquée, taille de son premier niveau) | taille du contenu (en-tête) |
| `.tex` animée | taille de l'atlas | taille d'une image (`gifWidth`, `gifHeight`) |
| cible de rendu | taille de création | taille de création |
| pochette média | (w, h) | (w, h) ; (1, 1, 1, 1) avant chargement |

Enregistrement :

- `g_Texture0Resolution` : recopié **à chaque image** depuis la texture effectivement liée à l'emplacement 0 (inchangé si aucune) ; valeur initiale = celle de la texture principale résolue à la création.
- `g_TextureNResolution` (N ≥ 1), pour chaque emplacement ayant une pile : résolution de la texture choisie **à la création** (sommet de pile, avec la règle `previous` → texture de l'emplacement traité juste avant). Si, à l'image, le repli du §8.4 choisit une autre texture, la résolution n'est pas mise à jour. Emplacement sans pile ⇒ uniform non renseigné (zéro).

### 8.6 Textures animées : `g_Texture0Rotation`, `g_Texture0Translation`

Uniquement si la texture liée en emplacement 0 est animée (planche d'images de type GIF). Soit t = horloge de rendu courante (même source que `g_Time`, lue au moment du dessin), T = somme des durées des images de la **texture principale de l'objet**, t' = t mod T (reste flottant). On parcourt les images dans l'ordre en retranchant leur durée à t' ; la première image pour laquelle le reste devient ≤ 0 est l'image courante, de numéro n, position (x, y), axes (w1, w2, h1, h2) dans l'atlas, et de taille d'atlas (W, H) :

- `g_Texture0Translation` = (x / W, y / H) ;
- `g_Texture0Rotation` = (w1 / W, w2 / W, h2 / H, h1 / H) (attention : h2 avant h1).

Texture non animée : les deux valent zéro (et sont tout de même téléversés à chaque image). Si T = 0, la première image est choisie.

### 8.7 Cibles de rendu nommées connues

| Nom | Taille | Remarque |
|---|---|---|
| `_rt_FullFrameBuffer` | taille de la scène | rendu courant de la scène ; pour les particules à combo `REFRACT`, une copie locale de même nom masque celle de la scène (copiée avant chaque dessin). |
| `_rt_MipMappedFrameBuffer` | — | alias de `_rt_FullFrameBuffer` |
| `_rt_imageLayerComposite_<id>_a`, `…_b` | taille de l'image | tampons ping-pong de l'image d'identifiant `<id>` (visibles par tous les objets de la scène) |
| `_rt_4FrameBuffer`, `_rt_8FrameBuffer` | scène / 4, scène / 8 | bloom |
| `_rt_Bloom` | scène / 8 | bloom |
| `_rt_shadowAtlas` | taille de la scène | jamais rempli (lumières non gérées) |
| `_alias_lightCookie` | — | alias de `_rt_shadowAtlas` |
| `fbos[].name` d'un effet | taille de l'image / `scale` | format toujours RGBA8, quel que soit `format` |

---

## 9. Assemblage de la source finale d'une unité

Dans l'ordre :

1. `#version 330`.
2. Lignes de commentaire (sans effet), puis `precision highp float;`.
3. Macros de compatibilité du §3.3, puis `#define GLSL 1` et une ligne vide.
4. Spécifique à l'unité (§3.3) : fragment ⇒ déclaration `out vec4 out_FragColor;` + `varying` ≡ `in` ; vertex ⇒ `attribute` ≡ `in` + `varying` ≡ `out`.
5. Défines de combos (§5.5).
6. Corps prétraité (§4), après les deux adaptations ci-dessous.

### 9.1 Adaptation des varyings élargis (unité vertex)

Pour chaque déclaration `varying vec4 NOM;` (espaces quelconques) présente dans l'unité **fragment** prétraitée : si l'unité vertex déclare `varying vec2 NOM;`, alors dans le vertex :

- la déclaration devient `varying vec4 NOM;` ;
- chaque instruction d'affectation simple **en début de ligne** (après indentation éventuelle) de la forme `NOM = expr;` (expr sur une seule ligne, sans `;`) devient `NOM = vec4(expr, 0.0, 1.0);`.

Les affectations composées, avec swizzle, ou non situées en début de ligne ne sont pas modifiées.

### 9.2 Adaptation de `v_TexCoord` (unité fragment)

Si l'unité fragment déclare `varying vec3 v_TexCoord;` ou `varying vec4 v_TexCoord;` et contient au moins une expression où l'identifiant entier `v_TexCoord` est directement combiné par `+ - * /` (espaces permis) avec un appel `CAST2(…)` (dans un sens ou dans l'autre), alors chacune de ces occurrences de `v_TexCoord` est remplacée par `v_TexCoord.xy`. Pour le sens `CAST2(…) op v_TexCoord`, l'argument de `CAST2` ne doit pas contenir de `)`.

---

## 10. Validation et traduction

### 10.1 Paramètres de la référence

- Les deux unités sont analysées comme GLSL 330, profil core, client OpenGL (sémantique OpenGL 4.5 pour la cible SPIR-V 1.5), avec affectation automatique des emplacements et liaisons, puis **liées ensemble** (validation inter-étapes), converties en SPIR-V, puis ré-émises en GLSL 330 desktop (non ES).
- Limites de ressources : jeu standard (32 unités de texture, 64 attributs de sommets, etc.), boucles et indexations générales autorisées.
- Les noms d'uniforms, d'attributs et de varyings sont conservés ; varyings appariés par nom ; aucun emplacement explicite n'est émis pour les uniforms.
- À titre de diagnostic, la source d'entrée est annexée à la sortie dans un bloc `#if 0 … #endif` ; sans effet fonctionnel, non requis.

Lumen PEUT utiliser une autre chaîne (cf. §16), mais DOIT accepter exactement le langage décrit au §10.2 et en conserver la sémantique.

### 10.2 Assouplissements du langage (à reproduire)

Le front-end de la référence est un GLSL standard modifié comme suit. Hors de ces points, les règles GLSL 330 core s'appliquent.

**R1 — Promotions implicites de type de base supplémentaires.**
- `bool` → `float` et `bool` → `double` : toujours (1.0 / 0.0). En particulier une opération arithmétique mixte `bool` / `float` est acceptée (le booléen est converti).
- `float`/`double` → `int`, `uint`, `int64`, `uint64`, `uint16` (troncature vers zéro) et `uint` → `int` : **uniquement** pour l'affectation simple `=`, les opérateurs `+ - * / %` et l'affectation composée `*=`. Pas pour `+=`, `-=`, `/=`, ni pour un argument de fonction ou un `return`. Dans une expression binaire mixte entier/flottant, le résultat reste flottant (règle standard) : la nouveauté sert surtout à rendre légal `int i = expr_flottante;` ou `i *= 0.5` (où 0.5 est d'abord converti en 0).

**R2 — Adaptations de forme implicites (même type de base uniquement).** Dans : affectations (`=`, `+=`, `-=`, `*=`, `/=`, `%=`, `&=`, `|=`, `^=`), arguments de fonctions (après résolution, R3), valeurs de `return`, séquences (virgule), constructeurs de structures, `!`, et arguments des fonctions intégrées `atan`, `clamp`, `cross`, `distance`, `dot`, `faceforward`, `fma`, `frexp`, `ldexp`, `mix`, `max`, `min`, `mod`, `modf`, `pow`, `reflect`, `refract`, `smoothstep`, `step` :
- scalaire → vecteur : diffusion sur toutes les composantes ;
- vecteur → scalaire : première composante ;
- vecteur N → vecteur M avec N > M : M premières composantes.
L'élargissement (vec2 → vec4 par ex.) reste une **erreur** ; matrices, tableaux et structures ne changent jamais de forme. La combinaison d'un changement de type de base **et** de forme en une seule étape n'est pas garantie (§17).

**R3 — Résolution des surcharges en deux phases** (fonctions utilisateur et intégrées) :
1. Phase stricte : règle GLSL standard (même forme exigée ; conversions de type de base = promotions standard plus `bool` → `float`/`double` ; jamais flottant → entier). Un seul candidat ⇒ retenu ; plusieurs ⇒ erreur d'ambiguïté.
2. Si aucun candidat : phase lâche. Un candidat de même nombre de paramètres convient si, pour chaque argument : type identique ; ou (ni l'un ni l'autre n'étant un tableau) forme identique ou argument adaptable vers le paramètre par diffusion scalaire→vecteur ou réduction vecteur→vecteur plus petit (même type de base), **et** type de base compatible dans le sens du paramètre (`in` : argument promouvable vers paramètre ; `out` : paramètre promouvable vers argument ; promotions de R1 **sans** les cas restreints aux opérateurs). Chaque diffusion/réduction compte pour une adaptation. Le candidat requérant le **moins** d'adaptations est retenu ; à égalité, le premier rencontré. Une erreur d'ambiguïté n'est émise que si au moins **trois** candidats partagent le minimum. Vecteur → scalaire n'est **pas** admis dans cette phase.

**R4 — Conditions entières.** Une expression scalaire entière (signée ou non, toute largeur) est acceptée comme condition de `if`, `while`, `for`, `do…while` et de l'opérateur ternaire. Pour un ternaire produisant une valeur, la condition est réécrite en `(cond > 0)` (comparaison du type de la condition : une valeur signée négative est **fausse**). Pour les instructions (`if`, boucles), l'amont n'insère aucune conversion (§17) ; Lumen DEVRAIT utiliser `cond != 0`.

**R5 — Opérateurs `%`, `&`, `|`, `^`, `<<`, `>>`** et leurs formes composées : si les deux opérandes ont le même type de base, l'opération est acceptée quel que soit ce type, avec le type de l'opérande gauche. Seul le cas utile est `%` sur flottants : il produit le modulo flottant à arrondi par défaut, `a - b * floor(a / b)` (équivalent GLSL `mod`).

**R6 — Indices flottants.** Une expression scalaire flottante est acceptée comme indice de tableau, de vecteur ou de matrice ; à l'émission, **tout** indice dynamique est converti en entier par troncature vers zéro.

**R7 — Swizzle hors limites.** Un sélecteur dépassant la taille du vecteur produit un avertissement et l'indice de composante est pris modulo la taille : sur un vec2, `z` → `x`, `w` → `y` ; sur un vec3, `w` → `x`.

**R8 — Varyings de tailles différentes entre étapes.** À l'édition de liens, si une sortie vertex et une entrée fragment de même nom ont le même type de base mais des tailles de vecteur différentes, les deux sont élargies à la plus grande taille et toutes leurs utilisations sont retypées. Types de base différents ⇒ erreur. Le même mécanisme s'applique, en principe, aux uniforms de même nom déclarés dans les deux étapes avec des tailles différentes (§17). Le cas vec2 (vertex) / vec4 (fragment) est déjà traité textuellement au §9.1, avec remplissage z = 0, w = 1.

### 10.3 Émission

La sortie est du GLSL 330 core : uniforms libres (bloc par défaut), samplers sans liaison explicite, attributs et varyings identifiés par leur nom, sortie fragment unique `out_FragColor`. Les indices dynamiques sont enveloppés dans une conversion entière (R6).

### 10.4 Échecs

- Échec d'analyse ou d'édition de liens dans la traduction : erreur journalisée, sources vides ⇒ échec de compilation GPU.
- Compilation ou édition de liens GPU : journal non vide avec statut OK ⇒ simple message d'erreur ; statut en échec ⇒ exception.
- Toute exception levée pendant la préparation d'un pass (prétraitement, annotations, combos, traduction, compilation, `bind` introuvable) fait **abandonner l'objet entier** (image ou particules), journalisé ; le reste de la scène est rendu.

---

## 11. Programme GPU et états du pass

### 11.1 Programme

Un programme par pass (vertex + fragment). Après l'édition de liens, les objets shader intermédiaires sont détachés et libérés.

### 11.2 États de rendu (depuis le pass matériau)

| Clé (défaut) | Valeur | Effet |
|---|---|---|
| `blending` (`normal`) | `normal` | mélange actif, facteurs couleur et alpha (1, 0) : remplacement |
| | `translucent` | couleur et alpha : (src_alpha, 1 − src_alpha) |
| | `additive` | couleur et alpha : (src_alpha, 1) |
| | autre | journalisé, traité comme `normal` |
| `depthtest` (`disabled`) | `enabled` | test de profondeur actif, fonction « inférieur ou égal » |
| `depthwrite` (`disabled`) | `enabled` | écriture de profondeur |
| `cullmode` (`nocull`) | `normal` | élimination des faces arrière (convention par défaut : faces avant en sens trigonométrique) |

Le mode de mélange effectif peut être modifié par la logique image/particules (cf. specs correspondantes).

---

## 12. Uniforms injectés par le moteur

### 12.1 Enregistrement et priorités

À la création du pass, les uniforms sont enregistrés **par nom**, dans cet ordre ; un enregistrement ultérieur du même nom remplace le précédent (sauf `g_CompositeColor`) ; un nom absent du programme final (non déclaré ou éliminé par le compilateur) est ignoré :

1. uniforms annotés avec clé matériau (vertex puis fragment), valeur par défaut, si le nom n'est pas déjà enregistré ;
2. constantes du pass matériau (§7.3) ;
3. constantes de la surcharge ;
4. samplers et résolutions (§8) ;
5. uniforms moteur (§12.2) ;
6. uniforms propres au type d'objet, ajoutés après la création (§12.3).

Conséquence : un uniform moteur l'emporte sur une annotation de même nom, **sauf** `g_CompositeColor`, que le moteur ne fournit que si aucune annotation/constante ne l'a déjà enregistré.

Deux modes de valeur :
- **figée** : copiée une fois à la création du pass ;
- **vive** : relue à chaque image.

Tout autre uniform `g_*` utilisé par un shader mais absent des tables ci-dessous reste à sa valeur initiale, zéro.

### 12.2 Table générale (images, effets, particules)

| Nom | Type | Valeur | Mode |
|---|---|---|---|
| `g_Time` | float | Secondes écoulées depuis le démarrage du pilote vidéo (§13.1). | vive |
| `g_Daytime` | float | (heure × 60 + minute) / 1440, heure locale (§13.2). | vive |
| `g_Texture0`…`g_Texture7` | sampler2D | Unités 0…7. | figée |
| `g_Texture0Resolution` | vec4 | §8.5 | vive |
| `g_Texture1Resolution`… | vec4 | §8.5 (pour chaque emplacement ayant une pile) | vive (pointe la texture choisie à la création) |
| `g_Texture0Rotation` | vec4 | §8.6 | vive |
| `g_Texture0Translation` | vec2 | §8.6 | vive |
| `g_TextureReductionScale` | float | 1.0 | figée |
| `g_Brightness` | float | Image : `brightness` de l'objet (défaut 1). Particules : constante `ui_editor_properties_overbright` du premier pass matériau (défaut 1). | figée |
| `g_UserAlpha` | float | Image : `alpha` de l'objet (défaut 1). Particules : `alpha` de `instanceoverride`. | figée |
| `g_Alpha` | float | Identique à `g_UserAlpha`. | figée |
| `g_Color` | vec3 | Image : `color` de l'objet (défaut blanc). Particules : `color` de `instanceoverride`, sinon (1, 1, 1). | figée |
| `g_Color4` | vec4 | Même couleur avec alpha (défaut (1, 1, 1, 1)). | figée |
| `g_CompositeColor` | vec3 | Comme `g_Color`, seulement si non déjà enregistré (§12.1). | figée |
| `g_LightAmbientColor` | vec3 | Couleur ambiante générale de la scène. | figée |
| `g_LightSkylightColor` | vec3 | Couleur « skylight » générale de la scène. | figée |
| `g_ModelViewProjectionMatrix` | mat4 | Matrice du pass (§13.5, §13.6). | vive |
| `g_ModelViewProjectionMatrixInverse` | mat4 | Son inverse. | vive |
| `g_EffectModelViewProjectionMatrix` | mat4 | Identique à `g_ModelViewProjectionMatrix`. | vive |
| `g_ModelMatrix` | mat4 | §13.5 / §13.6 | vive |
| `g_EffectModelMatrix` | mat4 | Identique à `g_ModelMatrix`. | vive |
| `g_ViewProjectionMatrix` | mat4 | Images : identité. Particules : §13.6. | vive |
| `g_NormalModelMatrix` | mat3 | Identité. | figée |
| `g_EffectTextureProjectionMatrix` | mat4 | Identité. | figée |
| `g_EffectTextureProjectionMatrixInverse` | mat4 | Identité. | figée |
| `g_PointerPosition` | vec2 | Pointeur en coordonnées normalisées de la scène (§13.3). | vive |
| `g_PointerPositionLast` | vec2 | Valeur de l'image précédente. | vive |
| `g_TexelSize` | vec2 | (1 / largeur_scène, 1 / hauteur_scène) — taille de la projection de la scène, pas de la cible. | figée |
| `g_TexelSizeHalf` | vec2 | (0,5 / largeur_scène, 0,5 / hauteur_scène) | figée |
| `g_AudioSpectrum16Left`, `g_AudioSpectrum16Right` | float[16] | §13.4 (gauche = droite : capture mono) | vive |
| `g_AudioSpectrum32Left`, `g_AudioSpectrum32Right` | float[32] | idem | vive |
| `g_AudioSpectrum64Left`, `g_AudioSpectrum64Right` | float[64] | idem | vive |

Les valeurs « figées » de couleur/alpha/luminosité ne suivent donc **pas** les modifications ultérieures de propriétés utilisateur ou de scripts (comportement amont, §17).

### 12.3 Uniforms propres aux particules

| Nom | Type | Valeur |
|---|---|---|
| `g_ModelMatrixInverse` | mat4 | inverse de la matrice modèle (§13.6) |
| `g_OrientationUp` | vec3 | (0, 1, 0) |
| `g_OrientationRight` | vec3 | (1, 0, 0) |
| `g_OrientationForward` | vec3 | (0, 0, 1) |
| `g_ViewUp` | vec3 | (0, 1, 0) |
| `g_ViewRight` | vec3 | (1, 0, 0) |
| `g_EyePosition` | vec3 | (0, 0, 1000) |
| `g_RenderVar0` | vec4 | (longueur de traînée, longueur max, longueur min, 0) |
| `g_RenderVar1` | vec4 | planche de sprites (c colonnes, r lignes, F images, atlas W×H) : (1/c, 1/r, F, (H·(1/r)) / (W·(1/c))) ; sinon (0, 0, 0, hauteur_réelle / largeur_réelle) ; ratio 1 si largeur nulle |
| `g_RefractAmount` | float | 0,05, seulement si le premier pass matériau a le combo `REFRACT` ≠ 0 |

Toutes vives. Les particules lient en outre l'emplacement 0 à `previous` (la texture de la particule) pour masquer un éventuel `default` du shader.

### 12.4 Téléversement par image

Ordre, à chaque dessin d'un pass : liaison du framebuffer cible et des états (§11.2) ; activation du programme ; liaison des textures et téléversement de `g_Texture0Rotation`/`g_Texture0Translation` ; téléversement de tous les uniforms valeur/vecteur/tableau ; puis des matrices liées au pass (en cas de doublon de nom, ces dernières l'emportent) ; attributs ; dessin ; déliaison.

- Tableaux : `count` éléments (16/32/64) ; pour vecteurs et matrices, un seul élément.
- Matrices : colonne-majeure, **non transposées** (convention `M * v` en GLSL, d'où `mul(v, M)` → `M * v`).
- Entiers via l'API entière, flottants via l'API flottante. Un uniform déclaré avec un type différent de celui téléversé n'est pas mis à jour (erreur silencieuse).

---

## 13. Calculs détaillés

### 13.1 `g_Time`

- Horloge monotone en secondes, flottant 32 bits, origine = initialisation du pilote vidéo (fenêtre ou Wayland). Elle n'est **pas** remise à zéro au changement de fond d'écran.
- Échantillonnée une fois par itération de la boucle principale, avant le rendu. Pendant une pause (fenêtre plein écran détectée), elle n'est plus mise à jour ; à la reprise elle **saute** à l'heure courante (pas de compensation de la durée de pause).
- Précision : avec un flottant 32 bits, la résolution se dégrade avec la durée de fonctionnement (≈ 8 ms après une journée).

### 13.2 `g_Daytime`

`(heure_locale × 60 + minute_locale) / (24 × 60)`, résolution d'une minute, dans [0 ; 1439/1440]. Mise à jour à chaque itération hors pause.

### 13.3 `g_PointerPosition`

Pour chaque scène, une fois par image avant le rendu, à partir de la position du pointeur en pixels (px, py), **py compté depuis le bas** de la surface (convention des pilotes d'entrée), et du viewport (vx, vy, vw, vh) :

- u = clamp((px − vx) / vw, 0, 1), v = clamp((py − vy) / vh, 0, 1) ;
- avec la fenêtre UV visible de la scène (u0, u1, v0, v1) (recadrage « fill/fit », cf. spec sortie) :
- `g_PointerPosition` = (u0 + u · (u1 − u0), v0 + (1 − v) · (v1 − v0)).

Donc (0, 0) = coin haut-gauche de la portion visible, (1, 1) = bas-droite. `g_PointerPositionLast` = valeur de l'image précédente (initialement (0, 0)).

### 13.4 Spectre audio

(Détails de capture : spec audio.) Capture du moniteur de la sortie par défaut, 8 bits non signés, mono, 44 100 Hz, par blocs de 1024 échantillons.

À chaque itération (une fois par image, après la mise à jour de `g_Time`) :

1. **Lissage** : chaque valeur affichée a_k se rapproche de sa cible c_k d'au plus 0,3 : si |c_k − a_k| ≤ 0,3 alors a_k = c_k, sinon a_k += 0,3 · signe(c_k − a_k).
2. Si un nouveau bloc complet est disponible (sinon fin) :
   - échantillons s_i = (u8_i − 128) / 128, i = 0…1023 ;
   - FFT réelle non normalisée de 1024 points → coefficients X_k (k = 0…512) ;
   - pour b = 0…63 : P = Re(X_{2b})² + Im(X_{2b})² ; f = 0,35 · log10(P) si P > 0, sinon 0 ;
     - c64[b] = min(1, f · (2 − e^((1 − b/63) − 0,5))) ;
     - c32[⌊b/2⌋] = min(1, f · (2 − e^((1 − b/31) − 0,5))) ;
     - c16[⌊b/4⌋] = min(1, f · (2 − e^((1 − b/15) − 0,5))) ;
     (chaque case de c32/c16 est écrasée successivement : la valeur retenue est celle du **dernier** b, soit b = 2j+1 pour c32[j] et b = 4j+3 pour c16[j]).
   - Aucun plancher : f est négatif si P < 1, donc les valeurs peuvent être négatives.
3. Les nouvelles cibles ne sont appliquées qu'à partir du lissage de l'itération suivante.

Si aucun fond d'écran chargé ne déclare le support du traitement audio, ou si le traitement audio est désactivé par l'utilisateur, toutes les valeurs restent à 0.

### 13.5 Matrices des images

(Géométrie et enchaînement : spec image.) Soit (w, h) la taille de l'image, O(l, r, b, t) la projection orthographique de near −1 / far 1 :

```
O(0, w, 0, h) = | 2/w   0    0   -1 |
                |  0   2/h   0   -1 |
                |  0    0   -1    0 |
                |  0    0    0    1 |
```

- **Premier pass** (texture de l'image → tampon composite) : MVP = O(0, w, 0, h) ; pour une image `passthrough`, MVP = matrice écran ci-dessous.
- **Passes intermédiaires** : MVP = identité.
- **Dernier pass** (dessiné dans la scène, si l'image est visible et que ce pass n'écrit pas dans une cible nommée) : MVP = P · V · R · Tp, où P = projection de la caméra de scène, V = matrice de vue de la caméra, R = rotation autour du centre c de l'image dans le plan : translation(c) · rotation_z(−angle) · translation(−c) (identité si angle = 0), Tp = translation parallaxe ((dx + a) · Dx · W, (dy + a) · Dy · W, 0) si la parallaxe est active (dx, dy = `parallaxDepth`, a = quantité de parallaxe, D = déplacement lissé, W = largeur de scène, pour les deux axes).
- Un pass qui écrit dans une cible nommée garde la matrice qu'il aurait eue (premier ou intermédiaire).
- Un pass unique est à la fois premier et dernier : la règle « dernier pass » l'emporte.
- `g_ModelMatrix` = O(0, w, 0, h) pour **tous** les passes ; `g_ViewProjectionMatrix` = identité.
- Inverses : inverse mathématique de la MVP correspondante.

### 13.6 Matrices des particules

(Détails : spec particules.) M = translation(o') · Tp' · rotation_z(−az) · rotation_y(ay) · rotation_x(−ax) · échelle(s), avec o' = (ox − W/2, H/2 − oy, oz) ; Tp' comme ci-dessus mais avec chaque composante de profondeur portée à une magnitude minimale de 0,65 (signe conservé, 0 → +0,65). VP = P · V de la caméra, ou, si le bit de valeur 4 des drapeaux de la particule est levé, perspective(fov, W/H, near, far) · vue depuis (0, 0, 1000) vers l'origine, haut (0, 1, 0). MVP = VP · M. `g_ModelMatrix` = M, `g_ViewProjectionMatrix` = VP. Recalculées à chaque image.

---

## 14. Cache et durée de vie

- **Aucun cache de shaders sur disque**, aucun partage de programmes : chaque pass relit ses fichiers, refait le prétraitement, la traduction et la compilation, même si un pass identique existe déjà.
- Dans un pass, la source finale d'une unité est calculée une seule fois.
- Les fichiers include ne sont pas mis en cache.
- L'état global de la bibliothèque de traduction est initialisé une seule fois pour le processus.
- Les textures nommées (§8.3) sont mises en cache **par nom**, globalement pour tous les fonds d'écran, pour toute la durée du processus.
- Si Lumen introduit un cache (prévu par l'architecture), sa clé DOIT couvrir tout ce qui influence la source finale des deux unités : contenu de tous les fichiers lus (y compris includes et `zcompat`), combos explicites, ensemble des emplacements « utilisés » (§5.3), nom de shader effectif, ainsi que la version du traducteur.

---

## 15. Récapitulatif des cas limites

| Situation | Comportement |
|---|---|
| `#include` dans un commentaire ou une branche inactive | Inclus quand même, hoisté avant `main`. |
| Include introuvable | Commentaire, pas d'erreur. |
| Include inclus deux fois | Copié deux fois (peut causer des redéfinitions). |
| Cycle d'includes | Amont : boucle infinie. Lumen : erreur. |
| Pas de `main` reconnaissable | Objet abandonné. |
| Directive `#require` sans nom | Laissée telle quelle → erreur de compilation. |
| `[COMBO]` sur la dernière ligne sans `\n` | Ignoré. |
| `[COMBO]` sans `combo`, ou `default` non entier | Objet abandonné. |
| Annotation JSON invalide | Ignorée (journal). |
| Uniform annoté sans `material` | Jamais initialisé (0). |
| Défaut `float` donné en chaîne | Partie entière seulement. |
| Défaut `vecN` mal formé ou numérique | Objet abandonné. |
| Sampler hors schéma `g_TextureN` | Index absurde ou abandon (amont) ; Lumen : ignorer. |
| Combo de texture requis avec `default` non numérique | Objet abandonné (amont). |
| Texture introuvable (hors `bind`) | Journalisée, emplacement sans ce candidat. |
| `bind` vers un FBO inexistant | Objet abandonné. |
| `default` sur `g_Texture0` | Remplace l'entrée du pass (sauf `bind` sur 0). |
| Uniform moteur et annotation de même nom | Moteur gagne (sauf `g_CompositeColor`). |
| Propriété modifiée après chargement | Constantes liées : suivent. `g_Alpha`, `g_Color`, `g_Brightness`… : figés. |
| Échec de traduction ou de compilation | Objet abandonné, scène continue. |

---

## 16. Notes de portage (non normatives)

- **Conventions OpenGL** : l'inversion de signe de `ddy` (§3.3) émule sur OpenGL l'axe vertical « vers le bas » de Direct3D, pour lequel les shaders Wallpaper Engine sont écrits. Sur un back-end aux conventions D3D (cas de wgpu), la sémantique visée est : `ddy(x)` = dérivée de x selon l'axe vertical de l'écran orienté vers le bas. Il faut vérifier que l'orientation des cibles de rendu de Lumen ne réintroduit pas d'inversion (§17).
- **Uniforms libres** : le dialecte ne déclare que des uniforms hors bloc ; une cible sans bloc par défaut doit les regrouper (par étape ou de façon commune) et conserver une table nom → (décalage, type) pour le téléversement par nom (§12). Un uniform déclaré dans les deux étapes est une seule entité logique.
- **Samplers** : chaque `sampler2D` doit pouvoir être associé à l'emplacement N d'après son nom `g_TextureN` (§8.1) ; sur une cible à samplers séparés, une paire texture + sampler par emplacement, avec les paramètres de filtrage et d'adressage de la texture (spec textures).
- **Constantes de dialecte** (§3.3) : peuvent être fournies par un préambule textuel identique, ou par une traduction équivalente, tant que la sémantique (y compris l'échange d'arguments de `max`, le `fmod` tronqué et `%` flottant à arrondi par défaut) est conservée.
- **Front-end permissif** : les règles R1 à R8 doivent être implémentées comme des modifications de comportement d'un front-end GLSL ; les tests de non-régression DEVRAIENT couvrir chaque règle séparément.

---

## 17. Questions ouvertes

1. **Logique `require`/`requireany` des combos de textures (§5.3)** : les deux règles paraissent inversées (requis quand une condition n'est *pas* satisfaite). Faut-il reproduire l'amont à l'identique ou appliquer l'interprétation probable (requis quand les conditions sont satisfaites) ? Impact : présence de `MASK` & co. quand aucune texture n'est fournie.
2. **Combo de texture requis avec un `default` nom de texture** (`"util/white"`) : l'amont échoue (objet abandonné). Reproduire l'abandon, ou prendre V = 1 ?
3. **Défaut `float` en chaîne lu comme entier** (`"0.5"` → 0) : reproduire (1:1) ou lire un flottant ?
4. **Uniform annoté sans `material`** : la référence n'applique jamais son `default`. Wallpaper Engine l'applique-t-il ?
5. **Valeurs figées** de `g_Alpha`, `g_UserAlpha`, `g_Color`, `g_Color4`, `g_Brightness`, `g_CompositeColor`, couleurs de lumière : faut-il les rendre vives (comportement probable de Wallpaper Engine avec les scripts / propriétés) ou les figer comme l'amont ?
6. **`TEX0FORMAT`** est déduit de la texture principale de l'objet même pour les passes d'effet, dont l'entrée est un tampon RGBA. Reproduire ?
7. **Conditions entières dans `if`/boucles (R4)** : l'amont ne convertit pas (sortie vraisemblablement invalide). Choisir `!= 0` (sémantique HLSL) ou `> 0` (cohérent avec le ternaire amont) ?
8. **R2 combiné à R1** (ex. `vec3 v = 1;` : entier → vecteur flottant) : accepté ou erreur ? Comportement amont non établi sans exécution.
9. **R8 appliqué aux uniforms** de même nom et tailles différentes entre étapes : effectif et sans effet de bord ? Et pour les varyings élargis autrement que vec2→vec4 : valeur des composantes ajoutées ?
10. **`sampler2DComparison`** n'est pas un type GLSL : est-il redéfini par un include de base (ex. en sampler d'ombre) ou ces shaders échouent-ils toujours ? Lumen doit-il le traiter comme `sampler2D` ?
11. **Includes dupliqués / inclus sous condition** : Wallpaper Engine déduplique-t-il ou respecte-t-il les `#if` ? L'amont non ; reproduire l'amont est le choix par défaut.
12. **Bornes d'insertion des includes** : quand le `#if` englobant est sur la première ligne, l'amont insère après le premier caractère. Confirmer que Lumen insère en début de texte.
13. **Uniforms Wallpaper Engine non fournis** (ex. éclairage dynamique, paramètres d'écran, `g_Frametime` éventuel) : liste exhaustive à établir depuis la documentation publique ; ils valent 0 dans l'amont.
14. **`g_ViewProjectionMatrix` = identité et `g_ModelMatrix` = projection orthographique locale pour les images** : probablement incohérent avec Wallpaper Engine (shaders 3D/perspective). Reproduire tel quel ?
15. **Inversion de `ddy` sur wgpu** (§16) : à valider visuellement sur un effet utilisant `ddy` (normales depuis hauteur, par ex.).
16. **Spectre audio** : valeurs négatives possibles et canaux gauche/droite identiques. Reproduire l'amont ou viser la stéréo de Wallpaper Engine ?
17. **Pause** : `g_Time` saute à la reprise. Reproduire, ou compenser la durée de pause ?
18. **Noms d'identifiants réservés** : la chaîne de traduction de l'amont peut renommer certains identifiants (préfixe `gl_`, doubles soulignés). Un uniform ainsi renommé ne serait plus adressable par nom ; aucun cas connu, à confirmer.
