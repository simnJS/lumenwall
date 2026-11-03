# Spec 06 — Scripting (SceneScript)

Statut : rédigée en clean-room à partir de la lecture du comportement de l'implémentation de référence
(révision amont `a0ddce5`, moteur JavaScript embarqué : QuickJS-ng, sous-module figé). Rien n'a été exécuté :
les comportements « effectifs » décrits plus bas sont **déduits** de la lecture, pas observés.
Ce document est **autosuffisant** pour l'implémenteur Rust (probablement via `rquickjs`). Il s'appuie sur les specs
voisines pour : le modèle de scène et le parsing JSON, la caméra, l'horloge globale, l'entrée souris, la source
média (MPRIS), le rendu des images et des textes.

Conventions :

- **WE** = Wallpaper Engine (produit officiel). « API SceneScript » = surface JS imposée par WE (noms d'objets
  globaux, méthodes, propriétés, événements) ; ces noms sont reproduits tels quels.
- `t_g` = temps global de rendu en secondes (`f32`), `t_g_prev` = sa valeur à la frame précédente, cf. spec horloge.
  `dt = t_g − t_g_prev`.
- « Valeur dynamique » = valeur typée d'une propriété de scène (cf. §2), partagée entre parseur, scripts et rendu.
- `f32` / `f64` : précision des calculs. JS ne manipule que des `f64` ; toute valeur stockée côté hôte est en `f32`.
- Marqueurs utilisés dans tout le document :
  - **[Réf]** : comportement normatif à implémenter.
  - **[Amont-effectif]** : ce que fait réellement l'amont (déduit), quand cela diffère de [Réf].
  - **[Amont-intention]** : ce que le code amont cherche manifestement à faire mais n'atteint pas.
  - **[WE]** : comportement documenté de WE, pour comparaison.

---

## 0. Vue d'ensemble et règle de fidélité

### 0.1 Deux mécanismes indépendants

L'amont contient **deux** mécanismes de script qui partagent le même moteur JS mais n'ont presque rien en commun :

| | M1 — scripts de propriété | M2 — scripts de couche texte |
|---|---|---|
| Déclencheur | toute propriété scriptable d'un objet image / texte / particules portant une clé `script` | uniquement la propriété `text` d'un objet texte, si elle porte une clé `script` |
| Forme d'évaluation | module ECMAScript (avec `import`/`export`) | script classique, source pré-traitée puis enveloppée dans une fonction anonyme exécutée immédiatement |
| Contrat | `update(value)` exporté → nouvelle valeur de la propriété | `init()` / `update()` / `destroy()` ; le texte est lu dans `thisLayer.text` |
| Globales | globales réelles (`engine`, `thisScene`, `thisLayer`, …) | plusieurs globales **masquées** par des imitations locales (§9.3) |
| Moment | chargement de la scène puis chaque frame, avant le dessin | pendant le dessin de l'objet texte, chaque frame où il est dessiné |
| État amont | **inopérant** (voir §0.2) | **fonctionnel** |

### 0.2 État réel de l'amont (résumé)

La lecture du code amont montre que M1 et la quasi-totalité des objets natifs sont **câblés de façon
défectueuse**. Conséquences effectives (détaillées dans chaque section) :

1. M1 : l'évaluation d'un module renvoie une *promesse* et non l'espace de noms du module ; la fonction `update`
   n'est donc jamais trouvée, et le résultat « absent » est interprété comme `null` : **la propriété scriptée est
   remise à zéro (type Null) au chargement puis à chaque frame**. Seul le code de premier niveau du module
   s'exécute (une fois).
2. Les modules `WEMath` et `WEColor` n'exposent aucun export utilisable (les exports sont posés avant d'être
   déclarés) : tout `import` nommé depuis eux fait échouer l'évaluation du module importateur.
3. Les objets `Vec2/Vec3/Vec4` : seuls les composants (`x`, `y`, `z`, `w`) sont accessibles ; toute autre
   propriété (y compris les méthodes `add`, `copy`, `toString`…) lève une exception. Les constructeurs ne sont pas
   exposés en global.
4. `thisLayer` (M1) et `thisScene.getLayer(...)` renvoient des objets vides sans aucun accesseur.
5. `createScriptProperties`, `engine.setTimeout`, `engine.setInterval` sont enregistrés avec une convention
   d'appel incompatible (le paramètre d'identification de l'instance est lu dans un registre non initialisé) :
   comportement **indéfini** (le plus probable : renvoi d'`undefined` ou exception).
6. M2 (texte dynamique) fonctionne, avec des imitations partielles de `engine` et `thisScene`.

### 0.3 Règle de fidélité appliquée dans cette spec

- **R1** — Comportement amont déterministe et fonctionnel → reproduit **à l'identique** (y compris ses
  bizarreries : mauvais mappages de `thisScene`, format de `console.log`, `localStorage` en mémoire, imitations M2…).
- **R2** — Comportement amont indéfini, plantage, ou fonctionnalité rendue inaccessible par un défaut de câblage
  évident → on spécifie **[Amont-intention]** (la sémantique que le code implémente une fois le câblage réparé),
  **sauf** si cette intention contredit elle-même le nom officiel de l'API (ex. `subtract` inversé), auquel cas on
  spécifie **[WE]** et on documente l'intention à titre informatif.
- **R3** — Fonctionnalité absente de l'amont → **non implémentée** dans [Réf] ; listée en §14.3.

Le tableau §14.1 récapitule tous les points où [Réf] s'écarte de [Amont-effectif] ; l'orchestrateur peut décider
d'un mode « fidélité stricte » qui suivrait [Amont-effectif] partout où il est déterministe.

---

## 1. Déclaration des scripts dans le JSON de scène

### 1.1 Forme d'une valeur scriptée

Toute valeur de propriété lue comme « réglage utilisateur » (valeur pouvant être liée à une propriété
utilisateur du projet) peut prendre la forme objet :

```json
"origin": {
  "value": "960 540 0",
  "script": "<source complète du script>",
  "scriptproperties": { "vitesse": 2.5, "couleur": { "user": "macouleur", "value": "1 0 0" } },
  "user": "nomProprieteProjet"
}
```

- `value` : **obligatoire** dès que la forme objet est utilisée (sinon erreur de parsing de l'objet). Sa valeur
  initiale est typée selon les règles de la spec données (chaîne « a b c » → vecteur de la taille correspondante,
  chaîne numérique → `Float`, nombre entier JSON → `Int`, flottant → `Float`, booléen → `Boolean`, `null` → `Null`,
  chaîne non numérique → `String` ; pour les couleurs, analyse couleur → `Vec4`).
- `script` : chaîne contenant **tout** le source JS. Absente ou `null` → pas de script.
- `scriptproperties` : lue **seulement** si `script` est présent et non nul. Si c'est un objet JSON, chaque entrée
  `nom → donnée` est elle-même analysée comme un réglage utilisateur complet (valeur simple, ou objet avec
  `value` et éventuellement `user`/condition) ; elle peut donc être liée en direct à une propriété utilisateur du
  projet et suivre ses changements. Tout autre type est ignoré (table vide).
- `user` : liaison de la valeur elle-même à une propriété utilisateur (cf. spec données). Si la propriété
  utilisateur change, la valeur est écrasée ; le script verra la nouvelle valeur au prochain appel.

Le script et ses `scriptproperties` sont attachés à la valeur dynamique ; le fait d'avoir un script ne change pas
la valeur initiale.

### 1.2 Propriétés réellement scriptées (M1)

Seules les valeurs **enregistrées** par un objet *scriptable* déclenchent l'exécution d'un script. Sont
scriptables : images, textes, systèmes de particules (s'ils sont effectivement créés). Ne le sont **pas** : sons,
objets génériques/inconnus/groupes, lumières, objets non créés (particules désactivées par réglage, objets dont
l'initialisation a échoué).

Enregistrement, dans cet ordre (le **premier** enregistrement d'un nom gagne, les suivants sont ignorés) :

1. Commun à tout objet scriptable : `origin` (origine de l'objet), `scale`, `angles`, `visible` — ces trois
   derniers désignent la valeur **générique de l'objet** (celle lue dans la partie commune de l'objet), pas la
   copie spécifique au type.
2. Image : `origin`*, `scale`*, `angles`*, `visible`*, `alpha`, `color`, `parallaxDepth`.
3. Texte : `color`, `alpha`, `origin`*, `scale`*, `visible`*, `pointSize` (clé JSON `pointsize`), `text`.
4. Particules : `scale`*, `angles`*, `visible`*, `parallaxDepth`.

(\* déjà enregistré à l'étape 1 → ignoré.)

> **Conséquence importante (R1).** Pour les images et les textes, le parseur produit pour `scale`, `angles` et
> `visible` **deux** valeurs dynamiques distinctes lues dans la même clé JSON : la valeur générique et la copie
> spécifique au type. Seule la valeur générique reçoit le script, alors que les moteurs de rendu image et texte
> lisent la copie spécifique. Un script sur `scale`/`angles`/`visible` d'une image ou d'un texte n'a donc **aucun
> effet visible** (pour les particules, se référer à la spec particules pour savoir quelle valeur est lue).
> `origin` est une valeur unique partagée → effet visible.

Chaque enregistrement réussi d'une valeur portant un script crée une entrée M1 identifiée par la clé
`"<nom>_<id de l'objet>"` (ex. `alpha_42`). Si cette clé existe déjà (deux objets de même identifiant), le second
script est ignoré.

**[Réf] Exception :** la propriété `text` d'un objet texte n'est **pas** traitée par M1 (elle l'est par M2, §9).
[Amont-effectif] : elle est traitée par les deux ; côté M1 cela n'a d'autre effet visible que l'exécution une
seconde fois du code de premier niveau (journaux, effets sur `shared`/`localStorage`), car la remise à Null
conserve la chaîne de texte (§2.3). [Réf] supprime ce doublon (cf. §14.1).

### 1.3 Valeurs scriptées ignorées

Les clés `script` présentes ailleurs (visibilité d'effet, constantes de shader, propriétés de couche
d'animation, valeurs de particules autres que celles ci-dessus, sons, réglages de caméra/scène…) sont parsées
mais **jamais exécutées**.

---

## 2. Valeurs dynamiques (rappel normatif minimal)

### 2.1 Types

`Null`, `Vec4`, `Vec3`, `Vec2`, `Float`, `Int`, `Boolean`, `String`. Une valeur stocke simultanément toutes ses
projections : `vec4`, `vec3`, `vec2` (`f32`), `float` (`f32`), `int` (`i32`), `bool`, `string`, et son type
courant.

### 2.2 Mise à jour depuis un type donné

| Mise à jour avec | vec4 | vec3 | vec2 | float | int | bool | string | type |
|---|---|---|---|---|---|---|---|---|
| `Float f` | (f,f,f,f) | (f,f,f) | (f,f) | f | tronc(f) | tronc(f) ≠ 0 | "" | Float |
| `Int i` | (i,i,i,i) | (i,i,i) | (i,i) | i | i | i ≠ 0 | "" | Int |
| `Boolean b` | b→1/0 partout | idem | idem | 1/0 | 1/0 | b | "" | Boolean |
| `Vec2 (x,y)` | (x,y,0,0) | (x,y,0) | (x,y) | x | tronc(x) | x ≠ 0 | "" | Vec2 |
| `Vec3 (x,y,z)` | (x,y,z,0) | (x,y,z) | (x,y) | x | tronc(x) | x ≠ 0 | "" | Vec3 |
| `Vec4 (x,y,z,w)` | (x,y,z,w) | (x,y,z) | (x,y) | x | tronc(x) | x ≠ 0 | "" | Vec4 |
| `String s` | 0 | 0 | 0 | 0 | 0 | faux | s | String |
| `Null` | 0 | 0 | 0 | 0 | 0 | faux | **inchangée** | Null |

- Si la valeur porte une *condition* (liaison conditionnelle à une propriété utilisateur, cf. spec données) et
  qu'on lui affecte une chaîne `s` : toutes les projections numériques valent 1 si `s` égale la chaîne de
  condition, 0 sinon ; type `Boolean` ; la chaîne est quand même mémorisée.
- Après toute mise à jour, les écouteurs de la valeur sont notifiés (propagation vers les valeurs qui lui sont
  liées). Les mises à jour issues d'un script portent l'étiquette « source = script » (sans effet observable
  actuellement).
- Conséquence : renvoyer un `Vec3` pour une couleur stockée en `Vec4` remet l'alpha (`w`) à 0.

### 2.3 Remise à Null

La remise à Null met toutes les projections numériques à 0 et `bool` à faux mais **laisse la chaîne intacte**.

---

## 3. Environnement d'exécution JavaScript

### 3.1 Moteur et instances

- Langage : ECMAScript moderne (modules, classes, `let`/`const`, Promise, Proxy, tableaux typés, `Math`, `Date`,
  `JSON`, `RegExp`…) tel que fourni par un contexte QuickJS-ng standard. Aucune API Web/OS (pas de `window`,
  `fetch`, `setTimeout` global, `require`, accès fichiers).
- **Un runtime + un contexte JS par instance de scène** (chaque fond d'écran de type scène actif). Rien n'est
  partagé entre deux scènes (ni `shared`, ni `localStorage`, ni minuteurs).
- Tout s'exécute sur le fil de rendu, de façon synchrone.
- **Aucune file de tâches n'est jamais vidée** : les réactions de promesses (`then`, reprise après `await`,
  `queueMicrotask`) ne s'exécutent jamais (R1, cf. §14.2).
- **Aucune limite** de temps d'exécution, de mémoire ou de pile n'est configurée (valeurs par défaut du moteur) :
  une boucle infinie bloque le rendu ; une récursion trop profonde lève l'erreur standard du moteur.

### 3.2 Ordre d'initialisation (à la construction de la scène, avant la création des objets)

1. Abonnement aux notifications de la source média (métadonnées et pochette, §8.4).
2. Création du runtime et du contexte.
3. Création des types natifs : vecteurs `Vec4`, `Vec3`, `Vec2` et type « couche » (`ILayer`).
4. Création des objets globaux natifs `engine`, `input`, `thisScene`, `console` et de la fonction globale
   `createScriptProperties`.
5. Création des modules natifs `WEMath` et `WEColor` et installation du résolveur de modules (§7).
6. Exécution d'un petit script de prélude (en mode script global) qui définit `localStorage`,
   `MediaPlaybackEvent` et une globale technique inutilisée ; une erreur à ce stade est journalisée.
7. Définition des globales `engine`, `input`, `thisScene`, `console`, `shared` (objet vide neuf).

### 3.3 Globales visibles

| Nom | Nature | Attributs de la propriété globale | M1 | M2 |
|---|---|---|---|---|
| `engine` | objet natif (§6.1) | énumérable, **non inscriptible**, non configurable | oui | masqué par imitation (§9.3) |
| `input` | objet natif (§6.2) | idem | oui | oui |
| `thisScene` | objet natif (§6.3) | idem | oui | masqué par imitation (§9.3) |
| `console` | objet natif (§6.5) | idem | oui | oui |
| `shared` | objet JS ordinaire vide | idem (l'objet lui-même est modifiable) | oui | oui |
| `createScriptProperties` | fonction native (§6.9) | idem | oui | masquée par imitation (§9.3) |
| `thisLayer` | objet couche (§6.4) | propriété ordinaire (inscriptible, configurable, énumérable), **réaffectée** par l'hôte | oui | masqué par objet local |
| `localStorage` | objet JS (§6.7) | ordinaire | oui | oui |
| `MediaPlaybackEvent` | objet JS de constantes (§6.8) | ordinaire | oui | oui |

- Les objets natifs `engine`, `input`, `thisScene`, `console`, ainsi que les objets couche, ont un **prototype
  nul** : pas de `toString`, `hasOwnProperty`, etc. ; leur conversion en chaîne échoue (TypeError).
- Les modules M1 étant des modules ES, ils sont en mode strict : réaffecter `engine`, `shared`… lève une
  `TypeError`.
- L'amont pose aussi quelques globales techniques préfixées par `__` (registre des couches texte, contexte
  temporel M2, données d'amorçage temporaires, table d'intervalles inutilisée). Elles ne font pas partie de l'API ;
  l'implémentation Rust est libre de leur mécanisme. Un script qui les lirait est hors contrat.

---

## 4. Conversions hôte ↔ JS

### 4.1 Hôte → JS (valeur dynamique → valeur JS)

| Type de la valeur dynamique | Valeur JS produite |
|---|---|
| Null | `null` |
| String | chaîne |
| Float | nombre (le `f32` élargi en `f64`, ex. 0.1 → 0.10000000149011612) |
| Int | nombre entier |
| Boolean | booléen |
| Vec2 / Vec3 / Vec4 | **vue vivante** `Vec2`/`Vec3`/`Vec4` (§5.1) liée à la valeur dynamique elle-même |

La dimension de la vue est fixée au moment de la conversion d'après le type courant. Une vue vivante lit la
valeur à chaque accès ; une écriture de composant modifie immédiatement la propriété (§5.2).

### 4.2 JS → hôte (valeur de retour de `update`, M1)

Soit `r` la valeur renvoyée, appliquée à la valeur dynamique `V` de la propriété :

1. Exception levée pendant l'appel → `V` inchangée ; rien n'est journalisé.
2. `undefined` ou `null` → `V` remise à Null (§2.3). [Réf] = [Amont-intention] ; voir §14.2 (Q-M1-3).
3. Nombre représenté comme entier 32 bits par le moteur (étiquette entière QuickJS) → mise à jour `Int`.
4. Booléen → mise à jour `Boolean`.
5. Autre nombre (flottant) → mise à jour `Float` (arrondi `f32`).
6. Chaîne → mise à jour `String`.
7. Objet (y compris une vue vecteur) : lire ses propriétés `x`, `y`, `z`, `w`.
   - `x` ou `y` n'est pas un nombre → **[Réf]** : journaliser une erreur et laisser `V` inchangée.
     [Amont-effectif] : erreur fatale (exception C++ non rattrapée dans la boucle de rendu).
   - `z` n'est pas un nombre → mise à jour `Vec2(x, y)`.
   - sinon `w` n'est pas un nombre → mise à jour `Vec3(x, y, z)`.
   - sinon → mise à jour `Vec4(x, y, z, w)`.
   - **[Réf]** : `z` et `w` sont bien lus. [Amont-intention] : `z` et `w` ne sont jamais convertis et valent 0
     (un `Vec3` renvoyé perd son `z`). Défaut évident → corrigé (R2).
   - Un nombre « est un nombre » s'il s'agit d'une valeur JS `number` (entière ou flottante) ; une lecture de
     composant inexistant sur une vue vecteur (ex. `z` d'un `Vec2`) compte comme « pas un nombre ».
8. Autres types (symbole, BigInt, fonction…) : les fonctions sont des objets → règle 7 ; symbole/BigInt → `V`
   inchangée.

Tout passage par les règles 2 à 7 notifie les écouteurs de `V`.

### 4.3 Vues vivantes et copies

- **Vue vivante** : `value` passé à `update`, valeurs lues via une couche (§6.4), valeurs de
  `scriptProperties` (§6.9), `thisScene.clearcolor/ambientcolor/skylightcolor`.
- **Copie indépendante** : résultat de `copy()`, de toute opération vectorielle, des fonctions `WEColor`, de
  `input.cursor*Position`, des couleurs des événements média, des vecteurs construits par script.
- Une copie vit tant que l'objet JS vit (libérée par le ramasse-miettes). Une vue vivante ne doit pas survivre à la
  scène (l'amont ne le garantit pas ; [Réf] : toute vue vivante dont la cible a disparu se comporte comme une copie
  figée ou lève une exception à l'accès — au choix de l'implémenteur, sans plantage).

---

## 5. Vecteurs `Vec2`, `Vec3`, `Vec4`

### 5.1 Représentation

Un vecteur est un objet JS de classe native de nom `Vec2`, `Vec3` ou `Vec4`, adossé à une valeur dynamique (vue
vivante ou copie possédée). Composants stockés en `f32`.

### 5.2 Accès aux composants

- Lecture de `x`, `y` (toutes dimensions), `z` (`Vec3`, `Vec4`), `w` (`Vec4`) : nombre = projection
  correspondante de la valeur adossée (`vec2`, `vec3` ou `vec4` selon la dimension de la vue) élargie en `f64`.
- Écriture de l'un de ces composants :
  - la valeur affectée doit être un `number`, sinon exception (pas de conversion depuis chaîne/booléen) ;
  - le nouveau vecteur = projection courante de la dimension de la vue, avec ce composant remplacé (arrondi
    `f32`) ; la valeur adossée est mise à jour **avec le type VecN** (§2.2) et notifie ses écouteurs ;
  - affectation de tout autre nom (composant hors dimension comme `w` sur un `Vec3`, ou nom arbitraire comme
    `v.foo = 1`) → exception (R1 ; aucune propriété n'est créée).
- Lecture de toute autre propriété :
  - **[Réf]** : résolution JS normale via le prototype (méthodes §5.5) ; propriété inconnue → `undefined`.
  - [Amont-effectif] : exception pour **tout** nom autre qu'un composant valide (méthodes comprises), ce qui
    rend aussi impossible la conversion en chaîne (`String(v)`, concaténation, `console.log(v)`).
- Énumération : aucune propriété propre ; `Object.keys(v)` → `[]`, `JSON.stringify(v)` → `"{}"` (R1 ; voir
  §14.2 Q-VEC-4).

### 5.3 Conversion d'un argument en vecteur de dimension N

Utilisée par les constructeurs et toutes les méthodes binaires :

| Argument | Résultat |
|---|---|
| nombre | vecteur dont tous les composants valent ce nombre (`f32`) |
| objet | lire `x`, `y`, `z`, `w`. `x` et `y` doivent être des nombres. Si N = 2 et `z` est un nombre → **rejet**. Si N ≤ 3 et `w` est un nombre → **rejet**. Composants absents/non numériques → 0. |
| autre (chaîne, booléen, `undefined`, `null`…) | **rejet** |

Un **rejet** lève une `TypeError` JS ([Réf]). [Amont-intention] : exception C++ traversant le moteur JS (plantage
ou comportement indéfini).

Remarque : un `Vec2` passé à une opération de `Vec3` est accepté (z = 0) ; un `Vec3` passé à une opération de
`Vec2` est rejeté.

### 5.4 Constructeurs

- **[Réf]** : `Vec2`, `Vec3`, `Vec4` sont des **globales** constructibles (`new VecN(...)`).
  - 0 argument → exception.
  - 1 argument → conversion §5.3 (nombre → répété ; objet → composants).
  - ≥ 2 arguments → **[WE]** composant par composant : `x = arg0`, `y = arg1`, `z = arg2`, `w = arg3`
    (manquants = 0 ; non-nombres → `ToNumber`). [Amont-intention] : seul le premier argument est utilisé
    (`new Vec3(1,2,3)` donnerait (1,1,1)) — contredit l'API officielle, donc écarté (R2).
  - Résultat : copie possédée.
- [Amont-effectif] : les constructeurs existent en interne mais ne sont exposés nulle part → `ReferenceError`.
- [WE] accepte aussi une chaîne (`"1 2 3"`) : non supporté (§14.3).

### 5.5 Méthodes du prototype

Toutes renvoient une **nouvelle copie** sauf mention ; `this` doit être un vecteur de la bonne dimension, sinon
exception. `a` = argument converti (§5.3). Calculs en `f32` sauf mention.

| Méthode | Arité exigée | [Réf] (sémantique WE) | [Amont-intention] si différente |
|---|---|---|---|
| `copy()` | — | copie de `this` | |
| `equals(o)` | 0 → renvoie `false` | `o` doit être un vecteur de même dimension (sinon exception) ; égalité **exacte** des composants `f32` | |
| `length()` | — | `sqrt(dot(this,this))` | |
| `lengthSqr()` | — | `dot(this,this)` | renvoie la longueur (non au carré) |
| `normalize()` | — | `this · (1/sqrt(dot(this,this)))` ; vecteur nul → composants NaN | |
| `add(a)` | exactement 1, sinon `undefined` | `this + a` | `a + this` (identique) |
| `subtract(a)` | idem | `this − a` | `a − this` |
| `multiply(a)` | idem | `this ⊙ a` (composante à composante) | |
| `divide(a)` | idem | `this ⊘ a` (composante à composante, IEEE : division par 0 → ±inf/NaN) | `a ⊘ this` |
| `dot(a)` | idem | **nombre** `Σ thisᵢ·aᵢ` | renvoie un vecteur dont tous les composants valent ce produit scalaire |
| `cross(a)` (`Vec3` seul) | idem | `this × a` | `a × this` |
| `mix(o, t)` | exactement 2, sinon exception ; `t` doit être un nombre, sinon exception | `this·(1−t) + o·t` (calcul en `f64`, arrondi `f32`) | `o·(1−t) + this·t` |
| `min(a)` / `max(a)` | exactement 1, sinon `undefined` | min/max composante à composante | |
| `abs()` | — | valeur absolue par composante | |
| `sign()` | — | −1 / 0 / +1 par composante (NaN → 0) | |
| `round()` | — | arrondi au plus proche, demi **éloigné de zéro** | |
| `floor()` / `ceil()` | — | partie entière inférieure / supérieure | |
| `toString()` | — | voir §5.6 | |

### 5.6 Format texte d'un vecteur (`toString`)

Composants de la valeur adossée, chacun formaté en notation décimale fixe à **6 décimales** (`1.500000`,
`-0.000000`, `nan`, `inf`, `-inf`), séparés par `", "`. La valeur est formatée selon son **type courant** :
Vec2 → 2 composants, Vec3 → 3, Vec4 → 4 ; si le type adossé n'est plus vectoriel : Float → un nombre au même
format, Int → entier décimal, Boolean → `1`/`0`, String → la chaîne, Null → message « conversion inconnue » (non
normatif).

### 5.7 Matrices

`Mat3`, `Mat4` : absents de l'amont (§14.3).

---

## 6. Objets globaux

### 6.1 `engine`

| Membre | Nature | Valeur / effet |
|---|---|---|
| `frametime` | accesseur lecture | `dt` (`t_g − t_g_prev`, calculé en `f32`) |
| `runtime` | accesseur lecture | `t_g` |
| `timeOfDay` | accesseur lecture | `(heure_locale·60 + minute_locale) / 1440`, recalculé à chaque frame (résolution : la minute, pas de secondes) |
| `AUDIO_RESOLUTION_16` / `_32` / `_64` | constantes | 16 / 32 / 64 |
| `setTimeout(cb, délai?)` | méthode | §10 |
| `setInterval(cb, délai?)` | méthode | §10 |
| `openUserShortcut(...)` | méthode | aucun effet, renvoie `undefined` |

- Affecter `frametime`, `runtime` ou `timeOfDay` lève une exception (valeur levée non spécifiée ; recommandé :
  `TypeError`). Les accesseurs sont non configurables.
- Pendant une pause de l'application, `t_g` n'avance pas (cf. spec horloge).
- Tous les autres membres WE sont absents (§14.3).

### 6.2 `input`

| Membre | Valeur |
|---|---|
| `cursorScreenPosition` | **nouvelle** copie `Vec2` à chaque lecture : position souris normalisée dans l'espace de texture de la scène, c.-à-d. `u = u_début + px·(u_fin − u_début)`, `v = v_début + py·(v_fin − v_début)`, où `px`, `py` ∈ [0,1] sont la position du curseur relative à la zone d'affichage (bornée à [0,1]) et `[u_début,u_fin]×[v_début,v_fin]` la fenêtre UV visible de la scène (modes remplissage/ajustement). Convention de l'axe Y : celle du sous-système d'entrée (cf. spec entrée ; sous le pilote fenêtre, Y = 0 en bas). Mise à jour une fois par frame, avant les scripts. |
| `cursorWorldPosition` | nouvelle copie `Vec3(0, 0, 0)` (non implémenté) |
| `cursorLeftDown` | `false` (non implémenté) |

Affectations → exception. [WE] : `cursorScreenPosition` est en coordonnées écran (cf. §14.2 Q-IN-1).

### 6.3 `thisScene` (objet scène global)

Accesseurs en lecture seule (affectation → exception) ; chaque lecture relit la valeur courante de la scène :

| Membre | Valeur renvoyée (R1, y compris les anomalies) |
|---|---|
| `bloom` | booléen « bloom activé » |
| `bloomstrength` | intensité du bloom **tronquée en entier** |
| `bloomthreshold` | seuil du bloom **tronqué en entier** |
| `clearenabled` | **booléen « bloom activé »** (anomalie amont, cf. §14.2) |
| `clearcolor` | vue vivante `Vec3` sur la couleur d'effacement de la scène |
| `ambientcolor` | vue vivante `Vec3` sur la couleur ambiante |
| `skylightcolor` | vue vivante `Vec3` sur la **couleur ambiante** (anomalie amont) |
| `fov`, `nearz`, `farz` | flottants de projection caméra |
| `camerafade` | booléen |
| `camerashake` | booléen ; `camerashakespeed`, `camerashakeamplitude`, `camerashakeroughness` : flottants |
| `cameraparallax` | booléen ; `cameraparallaxamount`, `cameraparallaxdelay`, `cameraparallaxmouseinfluence` : flottants |
| `getLayer(x)` | voir ci-dessous |

Écrire un composant des vues de couleur modifie la valeur de scène (avec passage au type `Vec3`) mais n'a pas
d'effet de rendu (la couleur d'effacement n'est lue qu'à la création de la scène).

`getLayer(x)` :

- nombre d'arguments ≠ 1 → exception ;
- `x` nombre → converti en entier 32 bits ; recherche de l'objet **créé** ayant cet identifiant ; absent ou non
  scriptable → `undefined` ; sinon un **nouvel** objet couche (§6.4) ;
- `x` chaîne → parcours des objets dans l'**ordre de rendu** ; le premier dont le nom est exactement `x` *et* qui
  est scriptable → nouvel objet couche ; les objets non scriptables de même nom sont sautés ; aucun → **exception** ;
- autre type → exception.
- L'ordre de rendu peut contenir des objets internes ajoutés par le moteur (passe de bloom) ; ils sont
  trouvables par leur nom interne (sans importance pratique).

### 6.4 Objets couche (`thisLayer` en M1, résultat de `getLayer`)

- **[Réf]** (= [Amont-intention]) :
  - lecture d'une propriété dont le nom figure dans la table des propriétés enregistrées de l'objet (§1.2 :
    `origin`, `scale`, `angles`, `visible`, plus `alpha`, `color`, `parallaxDepth` pour une image ; `color`,
    `alpha`, `pointSize`, `text` pour un texte ; `parallaxDepth` pour des particules) → conversion §4.1
    (vecteurs = vues vivantes ; écrire un composant modifie donc la propriété) ;
  - tout autre nom → `undefined` ;
  - affectation d'une propriété → **ignorée silencieusement** (aucune propriété créée, aucune erreur) ;
  - prototype nul.
- [Amont-effectif] : aucun accesseur n'est branché ; l'objet est vide à prototype nul ; toute lecture renvoie
  `undefined` ; une affectation crée une propriété JS ordinaire sans effet sur le rendu.
- Noms à la casse amont : `pointSize` (WE : `pointsize`), `parallaxDepth`.

### 6.5 `console`

- `log(...args)` : si aucun argument, ne fait rien. Sinon chaque argument est converti en chaîne (`ToString` JS)
  et les résultats sont **concaténés sans séparateur** (`console.log("a", 1)` → `a1`), puis écrits sur le journal
  standard (une ligne).
- `error(...args)` : idem vers le journal d'erreurs.
- Si la conversion d'un argument échoue (objet à prototype nul, objet dont `toString` lève…) : **[Réf]** utiliser
  une représentation de repli (ex. `[object]`) et effacer l'exception ; [Amont-effectif] : comportement indéfini
  (ligne tronquée ou vide, exception laissée en suspens).
- Aucune autre méthode (`warn`, `info`, `debug` → `undefined`, leur appel lève `TypeError`).

### 6.6 `shared`

Objet JS ordinaire vide, unique par scène, partagé par tous les scripts M1 et M2 de la scène. Aucune persistance.

### 6.7 `localStorage`

Objet JS ordinaire (prototype `Object`) fourni par le prélude, en **mémoire seulement**, perdu à la fermeture de
la scène, non partagé entre scènes :

| Méthode | Effet |
|---|---|
| `get(clé)` | clé convertie en chaîne ; renvoie la chaîne stockée ou `null` si absente |
| `set(clé, valeur)` | stocke `String(valeur)` (un objet devient `"[object Object]"`) |
| `remove(clé)` | supprime la clé |
| `clear()` | vide tout |

Les méthodes dépendent de `this` (un appel détaché échoue). [WE] : `delete` au lieu de `remove`, paramètre
`location`, constantes `LOCATION_GLOBAL`/`LOCATION_SCREEN`, valeurs de tout type, persistance (§14.3).

### 6.8 `MediaPlaybackEvent`

Objet de constantes : `PLAYBACK_STOPPED = 0`, `PLAYBACK_PLAYING = 1`, `PLAYBACK_PAUSED = 2`.

### 6.9 `createScriptProperties()` (M1) et `scriptProperties`

- **[Réf]** : `createScriptProperties()` renvoie un *constructeur de propriétés* doté des méthodes `addSlider`,
  `addCheckbox`, `addText`, `addCombo`, `addColor` et `finish`.
  - Chaque `addX(options)` renvoie le même constructeur (chaînage). Seuls `options.name` et `options.value` sont
    retenus (défaut déclaré) ; les autres champs (`label`, `min`, `max`, `integer`, `options`…) sont ignorés.
  - `finish()` renvoie un objet *propriétés de script* lié à la valeur dynamique de la propriété dont le module
    est **en cours d'évaluation ou d'exécution** ; s'il n'y en a pas → `undefined`.
  - Lecture `scriptProperties.nom` : si `nom` figure dans les `scriptproperties` JSON de la valeur → conversion
    §4.1 de la valeur courante (donc suit en direct une propriété utilisateur liée) ; sinon, si un défaut a été
    déclaré par `addX({name: nom, value})` → ce défaut ; sinon `undefined`. Tout nom (y compris `toString`) est
    traité ainsi.
  - Écriture → exception.
- [Amont-intention] : les défauts déclarés par `addX` sont ignorés ; et le contexte « module en cours » n'est
  positionné qu'**après** l'évaluation du module, si bien que `finish()` au premier niveau se lie à la propriété du
  module **précédemment** chargé (ou renvoie `undefined` pour le tout premier).
- [Amont-effectif] : appel enregistré avec une convention incompatible → comportement indéfini (le plus probable
  `undefined`, d'où `TypeError` sur `.addSlider` et échec silencieux de l'évaluation du module).

---

## 7. Modules importables

### 7.1 Résolution

- Les modules M1 peuvent écrire `import { … } from 'WEMath'` / `'WEColor'`. Le nom est comparé **exactement**
  (sensible à la casse). Tout autre spécificateur (y compris `WEVector`, chemins relatifs) → échec de résolution →
  l'évaluation du module importateur échoue.
- Les modules natifs sont uniques par scène et partagés par tous les importateurs.
- En M2, toute instruction `import` est une erreur de syntaxe (le source n'est pas évalué comme module, §9.2).
- [Amont-effectif] : les deux modules sont déclarés sans exports effectifs → tout import nommé échoue à
  l'édition de liens ; un import d'espace de noms donne des membres `undefined`. [Réf] = exports fonctionnels.

### 7.2 `WEMath`

| Export | Définition |
|---|---|
| `smoothStep(e0, e1, x)` | exactement 3 arguments, tous `number`, sinon exception. `t = clamp((x−e0)/(e1−e0), 0, 1)` ; résultat `t·t·(3 − 2t)` en `f64`. Si `e0 = e1` : NaN se propage (le `clamp` ne filtre pas NaN). |
| `mix(a, b, t)` | exactement 3 `number`, sinon exception. `a·(1−t) + b·t` en `f64`. (Propriété `length` de la fonction = 1.) |
| `deg2rad` | nombre `π/180` |
| `rad2deg` | nombre `180/π` |

### 7.3 `WEColor`

Toutes : exactement 1 argument qui doit être un objet, sinon exception. Résultat : nouvelle copie `Vec3`
(composants arrondis `f32`). `x, y, z` = lecture des propriétés homonymes de l'argument converties par `ToNumber`
(`undefined` → NaN).

**`rgb2hsv(c)`** — entrée RGB dans [0,1], sortie (h en **degrés** [0,360), s ∈ [0,1], v) :

1. `M = max_f32(x, y, z)`, `m = min_f32(x, y, z)` : chaque composant est d'abord arrondi en `f32` ; si un
   opérande est NaN, max/min renvoient l'autre.
2. `v = M`.
3. Si `M = 0` ou `M − m = 0` : `h = 0`, `s = 0`.
4. Sinon `s = (M − m)/M` (`f32`) puis, en comparant `M` (`f32`) aux composants **non arrondis** (`f64`) :
   - si `M == x` : `h = 60·(y − z)/(M − m)` ;
   - sinon si `M == y` : `h = 60·(z − x)/(M − m) + 120` ;
   - sinon : `h = 60·(x − y)/(M − m) + 240` ;
   (calcul en `f64`, résultat arrondi `f32`). Conséquence à reproduire : un composant non représentable
   exactement en `f32` (ex. objet littéral `{x: 0.1, …}`) ne « gagne » jamais la comparaison et tombe dans la
   branche suivante.
5. Si `h < 0` : `h += 360`.

**`hsv2rgb(c)`** — entrée (h en degrés, s, v) :

1. `k = tronc(h/60)` (entier signé, troncature vers zéro) ; `i = k mod 6` avec le **signe du dividende**
   (reste tronqué : `−1 mod 6 = −1`).
2. `f = h/60 − i` (attention : `i` après modulo ; pour `h ∈ [360, 420)`, `f ∈ [6, 7)`).
3. `p = v·(1 − s)`, `q = v·(1 − s·f)`, `t = v·(1 − s·(1 − f))` (calcul `f64`, stockage `f32`).
4. `i = 0 → (v, t, p)` ; `1 → (q, v, p)` ; `2 → (p, v, t)` ; `3 → (p, q, v)` ; `4 → (t, p, v)` ;
   `5 → (v, p, q)` ; autre (négatif) → `(0, 0, 0)`.

**`normalizeColor(c)`** : `x`, `y`, `z` doivent être des `number` (sinon exception) ; résultat `(x/255, y/255, z/255)`.

**`expandColor(c)`** : idem ; résultat `(x·255, y·255, z·255)`.

### 7.4 `WEVector`

Absent (§14.3).

---

## 8. Cycle de vie M1 (scripts de propriété)

### 8.1 Chargement

Moment : pendant la construction de la scène, à la création de chaque objet scriptable (les objets sont créés
dans l'ordre du fichier, dépendances et parent d'abord — cf. spec scène), au moment où chaque propriété est
enregistrée (§1.2), c.-à-d. **avant** l'initialisation graphique de l'objet.

Pour chaque nouvelle entrée M1 (clé `nom_id`) **[Réf]** :

1. Positionner le contexte « module en cours » sur cette entrée et `thisLayer` sur un nouvel objet couche de
   l'objet propriétaire.
2. Évaluer le source comme **module ES** nommé par la clé (le nom n'a d'importance que pour les messages
   d'erreur). Le code de premier niveau s'exécute (une fois).
3. Si l'évaluation échoue (syntaxe, import introuvable, exception au premier niveau) : journaliser l'erreur, ne
   conserver aucun gestionnaire pour cette entrée, **laisser la valeur intacte**. [Amont-intention] : rien n'est
   journalisé et la valeur est remise à Null ensuite (voir 8.7).
4. Récupérer les exports du module (son espace de noms) et les conserver avec une référence à la valeur dynamique.
5. Si `update` est exporté et est une fonction : l'appeler **immédiatement** une première fois avec
   `value` = conversion §4.1 de la valeur, puis appliquer le résultat (§4.2).
6. `init` et `destroy` exportés ne sont **pas** appelés (R3 ; cf. §14.2 Q-M1-1).

### 8.2 Exécution par frame

Une fois par frame et par scène (même si la scène est affichée sur plusieurs écrans), au début du dessin de la
scène, après la mise à jour de la position souris et de la parallaxe, avant la mise à jour des textures et le
dessin des objets :

1. Minuteurs (§10) : d'abord les intervalles, puis les délais.
2. Pour chaque entrée M1, dans l'**ordre lexicographique des clés** `nom_id` (comparaison d'octets : `alpha_12` <
   `origin_3` < `scale_10` < `scale_2`) :
   - positionner « module en cours » et **[Réf]** `thisLayer` sur l'objet propriétaire de l'entrée
     ([Amont-intention] : `thisLayer` reste celui du **dernier** script chargé pendant toute la vie de la scène) ;
   - si `update` est une fonction : l'appeler avec `this` = espace de noms du module et un argument (§4.1),
     appliquer le résultat (§4.2). Sinon ne rien faire.

### 8.3 Application au rendu

Les valeurs dynamiques étant partagées, les moteurs de rendu lisent à chaque frame la valeur modifiée (dans la
limite du §1.2 : seule la valeur enregistrée est modifiée). Il n'y a pas de recalcul particulier déclenché par le
script : tout ce que le rendu relit par frame (origine → position, alpha, couleur, taille de police, visibilité
lue par le rendu…) suit ; ce qu'il ne lit qu'à la création ne suit pas.

### 8.4 Événements média

Déclenchement : chaque fois que la source média signale un changement de **métadonnées** (titre, artiste, durée,
état de lecture, position) **ou** de **pochette** (album, URL de pochette) — les deux déclenchent exactement le
même traitement. Ces signaux arrivent pendant la mise à jour de la source média, en début de frame, avant le
dessin. Aucun envoi initial à l'abonnement.

Traitement **[Réf]** (= [Amont-intention]) : construire **une seule fois** quatre objets JS ordinaires :

| Événement | Champs |
|---|---|
| propriétés | `title`, `artist`, `albumTitle` (chaînes) |
| lecture | `state` (entier 0 arrêt / 1 lecture / 2 pause) |
| chronologie | `position`, `duration` (nombres, **unités brutes de la source** : microsecondes MPRIS) |
| vignette | `hasThumbnail` (vrai dès qu'une URL de pochette — même vide — est connue), `primaryColor` = Vec3(0.12, 0.12, 0.12), `secondaryColor` = Vec3(0, 0, 0), `tertiaryColor` = Vec3(0.25, 0.25, 0.25), `highContrastColor` = Vec3(1, 1, 1) (copies, couleurs codées en dur) |

puis, pour chaque entrée M1 dans l'ordre des clés, appeler si exportées : `mediaPropertiesChanged(propriétés)`,
`mediaPlaybackChanged(lecture)`, `mediaTimelineChanged(chronologie)`, `mediaThumbnailChanged(vignette)`, dans cet
ordre. Les **mêmes** objets sont passés à tous les modules (une mutation par un module est vue par les suivants).
Valeurs de retour ignorées, exceptions ignorées sans journal. Le contexte « module en cours » n'est pas modifié ;
`thisLayer` **[Réf]** : positionné comme en 8.2.

M2 ne reçoit aucun événement média.

### 8.5 Événements non pris en charge

Jamais appelés (R3) : `init`, `destroy`, `resizeScreen`, `applyUserProperties`, `applyGeneralSettings`,
`cursorEnter`, `cursorLeave`, `cursorMove`, `cursorDown`, `cursorUp`, `cursorClick`, `mediaStatusChanged`.
Les changements de propriétés utilisateur ne parviennent aux scripts que par les valeurs vivantes (`value`,
`scriptProperties`).

### 8.6 Fin de vie

À la destruction de la scène : désabonnement média, libération des modules et du contexte. Aucun hook n'est
appelé pour M1.

### 8.7 [Amont-effectif] de M1, pour mémoire

L'évaluation d'un module renvoie une promesse ; la recherche de `update` sur cette promesse donne `undefined` ;
l'absence de fonction est traitée comme un résultat `undefined` ; la valeur est donc **remise à Null** au
chargement puis à chaque frame. Effets visibles typiques : origine scriptée ramenée à (0,0,0), alpha scripté à 0
(objet invisible), couleur scriptée noire (alpha 0 pour une couleur `Vec4`) ; `scale`/`angles`/`visible` sans
effet (cf. §1.2) ; texte : chaîne conservée. Les gestionnaires média ne sont jamais appelés. Si le module échoue
à l'évaluation, même résultat (remise à Null), sans journal.

---

## 9. Cycle de vie M2 (couches texte dynamiques)

### 9.1 Conditions de création

À l'initialisation graphique d'un objet texte dont la valeur `text` porte un script, **après** le chargement de
la police et la création de la première texture de glyphes (texte initial, ou un espace si la chaîne est vide).
Si la police ne peut être chargée, aucune couche n'est créée et le texte n'est pas dessiné (cf. spec texte).

### 9.2 Pré-traitement du source (R1, à reproduire tel quel)

Sur le source brut, dans cet ordre, chaque suppression étant répétée jusqu'à disparition :

1. supprimer toute occurrence littérale de la directive stricte entre apostrophes **suivie d'un point-virgule**
   (13 caractères) ;
2. idem entre guillemets doubles ;
3. supprimer toute occurrence de la séquence de 7 caractères « `export` + espace », **où qu'elle soit** (y compris
   dans une chaîne, un commentaire, ou la fin d'un identifiant comme `myexport x`).

Conséquences : le code s'exécute en mode **non strict** ; `export default …` devient `default …` (erreur de
syntaxe) ; `export\tfunction` (tabulation) reste et provoque une erreur ; `import` provoque une erreur de syntaxe.

### 9.3 Environnement local d'une couche

Le source pré-traité est placé dans le corps d'une fonction anonyme exécutée immédiatement, en mode script
global. Chaque couche a donc sa propre portée pour ses déclarations de premier niveau. Avant le source, la portée
locale définit :

- **props** : copie superficielle d'un objet `nom → valeur` construit à partir des `scriptproperties` de la valeur
  `text` (conversion §4.1, donc vecteurs = vues vivantes ; mais un nombre/chaîne est **figé** : pas de suivi des
  propriétés utilisateur après création).
- `thisLayer` : objet ordinaire `{ text: String(texte initial) }` (texte initial = chaîne de la valeur `text` au
  moment de la création, ou `""`). Masque la globale.
- `thisScene` : objet ordinaire avec quatre accesseurs en lecture lisant le contexte temporel de la dernière
  frame : `time` et `currentTime` (= `t_g`), `dt` (= `dt`), `fps` ; avant le premier tick : 0, 0, 0 et **60**.
  Masque la globale (pas de `getLayer`, `bloom`, etc.).
- `engine` : objet ordinaire avec deux accesseurs : `frametime` (= `dt` du contexte, 0 avant le premier tick) et
  `time` (= `t_g`). **Aucun autre membre** : `engine.runtime`, `timeOfDay`, `setTimeout`… valent `undefined`.
- `createScriptProperties()` : renvoie un constructeur en JS pur dont `addSlider`, `addCheckbox`, `addCombo`,
  `addColor`, `addText` prennent un objet options et, si `options.name` n'est pas déjà une clé de *props*, y
  écrivent `options.value` ; elles renvoient le constructeur. `finish()` renvoie l'objet *props* lui-même (objet
  ordinaire, modifiable par le script, pas de vue vivante).

Restent accessibles les globales `input`, `console`, `shared`, `localStorage`, `MediaPlaybackEvent`, ainsi que
les constructeurs vecteurs si exposés ([Réf] §5.4).

Après le source, la fonction enregistre, pour cette couche, dans un registre interne : l'objet `thisLayer`,
l'objet `thisScene`, et les fonctions `init`, `update`, `destroy` **si** les identifiants correspondants désignent
des fonctions dans la portée (test de type qui ne lève pas si l'identifiant n'existe pas).

Toute redéclaration de premier niveau par `let`/`const`/`class` des noms locaux ci-dessus provoque une erreur de
syntaxe (échec de création).

### 9.4 Création et identifiants

- Chaque tentative de création consomme un identifiant entier strictement croissant à partir de 1 (même en cas
  d'échec).
- Si l'évaluation lève (syntaxe, exception au premier niveau) : l'erreur est journalisée (journal d'erreurs, avec
  un libellé de contexte « création de couche » et le texte de l'exception), la couche n'est pas enregistrée, puis
  l'objet texte journalise l'échec avec son nom. Le texte reste alors figé sur le texte initial.
- Sinon la couche est marquée « non initialisée ».

### 9.5 Tick d'une couche

Appelé pendant le dessin de l'objet texte, **seulement si** l'objet est valide et que sa visibilité (valeur
`visible` spécifique au texte) est vraie — un texte invisible ne fait pas avancer son script. Les textes sont
dessinés dans l'ordre de rendu, donc après toutes les mises à jour M1 de la frame.

1. Mettre à jour le contexte temporel partagé : `time = t_g`, `dt = t_g − t_g_prev`,
   `fps = 1/dt` si `dt > 10⁻⁶`, sinon `60` (valeurs `f32` élargies en `f64`).
2. Si la couche est « non initialisée » : appeler `init()` (si enregistrée) avec `this` = l'enregistrement interne
   de la couche, sans argument ; la marquer « initialisée » **même si `init` lève**.
3. Si `update` est enregistrée : l'appeler avec `this` = l'enregistrement interne et **un** argument = la valeur
   courante de `thisLayer.text` ; si la valeur renvoyée est de type chaîne, l'affecter à `thisLayer.text` ; sinon
   (`undefined`, nombre…) ne rien faire. Les deux conventions WE sont donc supportées : mutation de
   `thisLayer.text`, ou `return` d'une chaîne.
4. Une exception dans `init` ou `update` est journalisée (libellé de contexte + texte de l'exception) et n'arrête
   pas les ticks suivants.

### 9.6 Lecture du texte et rendu

Juste après le tick : lire `thisLayer.text` de la couche ; `undefined`/`null` → `""` ; sinon conversion en chaîne
(`ToString`). Une chaîne vide est remplacée par un espace. Si la chaîne diffère de la dernière chaîne dessinée (ou
si la taille de rendu a changé), la texture de glyphes est régénérée (cf. spec texte). La valeur dynamique `text`
n'est **pas** modifiée par M2.

### 9.7 Destruction

À la destruction de l'objet texte (donc avant la fin du moteur JS) : appeler `destroy()` si enregistrée (même
`this`, sans argument ; exception journalisée), puis retirer la couche du registre.

### 9.8 Isolation

Les fonctions de premier niveau de deux couches ne se gênent pas (portées distinctes). Mais une affectation à un
identifiant non déclaré crée une **globale** (mode non strict) visible de tous les scripts.

---

## 10. Minuteurs `engine.setTimeout` / `engine.setInterval` (M1 uniquement)

[Réf] = [Amont-intention] corrigée des défauts de gestion mémoire et de la convention d'appel (R2) ; la fonction
d'annulation suit [WE].

- Signature : `setTimeout(callback, délai?)`, `setInterval(callback, délai?)`.
  - Aucun argument → exception. `callback` non fonction → exception.
  - `délai` : `ToInt32` du 2ᵉ argument s'il est présent, sinon 0 ; en millisecondes. **[Réf]** : délai négatif
    traité comme 0 ([Amont-intention] : conversion en entier non signé → débordement, comportement indéfini).
  - Le callback est **retenu** (empêché d'être collecté) jusqu'à son dernier déclenchement ou son annulation.
- Identifiants : deux compteurs distincts (délais, intervalles), commençant à 1, incrémentés à chaque création.
- Échéance initiale = instant de création (horloge monotone) + délai.
- Valeur renvoyée : une **fonction d'annulation** (longueur déclarée 2).
  - **[Réf]** : l'appeler (avec n'importe quels arguments) annule le minuteur qui l'a produite ; sans effet s'il
    est déjà terminé ou annulé ; renvoie `undefined`.
  - [Amont-intention] : exige exactement un argument (sinon exception) et annule le minuteur **dont l'identifiant
    est cet argument** (même type que le minuteur d'origine), en ignorant le minuteur associé.
- Déclenchement, au début du tick de scène (§8.2), avec `maintenant` = horloge monotone lue **une fois** :
  1. Intervalles, par identifiant croissant : si `échéance ≤ maintenant` → `échéance = maintenant + délai` (pas de
     rattrapage : au plus un appel par tick, dérive acceptée), puis appel `callback()` avec `this = null`, sans
     argument.
  2. Délais, par identifiant croissant : si `échéance ≤ maintenant` → appel `callback()` (`this = null`), puis
     suppression.
  - Valeurs de retour ignorées ; exceptions ignorées sans journal.
  - Un minuteur créé pendant ce tick (y compris dans un callback) ne peut pas se déclencher dans le même tick
    (son échéance est postérieure à `maintenant`). Délai 0 → déclenchement au tick suivant.
  - **[Réf]** : annuler un minuteur depuis un callback (y compris lui-même) est sûr ; un intervalle annulé n'est
    plus appelé ; un délai annulé avant son tour n'est pas appelé.
- Résolution effective : une frame.
- [Amont-effectif] : convention d'appel incompatible → comportement indéfini (probablement exception à chaque
  appel) ; callback non retenu (risque d'utilisation après libération).
- M2 : `engine` y est l'imitation locale → `engine.setTimeout` vaut `undefined`.

---

## 11. Gestion des erreurs (récapitulatif)

| Situation | [Réf] | [Amont-effectif] si différent |
|---|---|---|
| Échec du prélude | journal d'erreurs | |
| M1 : évaluation du module échoue | journal d'erreurs ; valeur intacte ; entrée inerte | silencieux ; valeur remise à Null chaque frame |
| M1 : `update` lève | silencieux ; valeur intacte | (jamais appelé) |
| M1 : `update` renvoie un objet sans `x`/`y` numériques | journal d'erreurs ; valeur intacte | erreur fatale |
| M1 : gestionnaire média lève | silencieux | (jamais appelé) |
| Callback de minuteur lève | silencieux | |
| Argument vecteur invalide | `TypeError` | exception native traversant le moteur (plantage probable) |
| Fonction native appelée avec mauvaise arité/type | exception (valeur levée non spécifiée ; recommandé `TypeError` avec message) | exception sans objet d'erreur |
| M2 : création échoue | journal d'erreurs (contexte + message) puis journal « échec de création » avec le nom de l'objet | |
| M2 : `init`/`update`/`destroy` lève | journal d'erreurs (contexte + message), on continue | |
| Boucle infinie | blocage du rendu (aucun garde-fou) | |

« Silencieux » = l'exception est consommée ; elle ne doit pas réapparaître plus tard dans un autre message
([Amont-effectif] : elle peut rester en suspens dans le moteur jusqu'à la prochaine exception).

---

## 12. Séquence d'une frame (vue d'ensemble)

1. Application : heure du jour (`timeOfDay`), `t_g_prev ← t_g`, `t_g ← temps de rendu`.
2. Mise à jour audio, puis **source média** (→ événements média M1, §8.4), puis entrée, puis événements fenêtre.
3. Pour chaque scène à dessiner (une seule fois par frame même sur plusieurs écrans) :
   1. position souris normalisée (§6.2), parallaxe ;
   2. **tick JS** : minuteurs (§10) puis `update` M1 (§8.2) ;
   3. mise à jour des textures animées ;
   4. dessin des objets dans l'ordre de rendu ; chaque texte scripté visible exécute son **tick M2** (§9.5) au
      moment de son dessin.

Au chargement de la scène : construction du moteur JS (§3.2), puis création des objets (chargement M1 au fil de
l'eau, §8.1 ; création M2 à l'initialisation de chaque texte, §9.1).

---

## 13. Notes d'implémentation Rust (non normatives)

- `rquickjs` repose sur QuickJS-ng : mêmes étiquettes de valeur (entier 32 bits vs flottant), ce qui permet de
  reproduire la règle §4.2-3/5 en testant le type de la valeur (`Int` vs `Float`).
- Évaluer un module renvoie une promesse : utiliser la déclaration puis l'évaluation de module, faire progresser la
  promesse (un passage de jobs suffit pour un module sans `await` de premier niveau), puis lire l'**espace de noms**
  du module pour obtenir `update` et les autres exports. Ne pas vider la file de jobs en dehors de ce besoin si l'on
  veut rester fidèle à §3.1 (cf. Q-RT-1).
- Vecteurs : une classe native par dimension, avec accesseurs `x/y/z/w` sur le prototype (pas de propriétés
  propres, pour garder `Object.keys` vide) et méthodes du §5.5 ; l'état interne = soit une copie possédée, soit une
  poignée vers la valeur dynamique de la scène (identifiant dans une table, ou pointeur partagé à durée contrôlée).
- Objets couche / propriétés de script : un `Proxy` JS, ou une classe native avec intercepteur, sur une poignée
  d'objet de scène ; les noms sont dynamiques.
- Modules natifs : déclarer `WEMath`/`WEColor` comme modules natifs avec un résolveur n'acceptant que ces deux
  noms exacts.
- M2 : construire la fonction englobante en Rust (texte généré) ou, de façon équivalente, évaluer le source
  pré-traité dans une fonction recevant les objets locaux en paramètres ; l'important est la sémantique §9.3
  (portée par couche, mode non strict, masquage de `engine`/`thisScene`/`thisLayer`/`createScriptProperties`).
- Les objets natifs à prototype nul (§3.3) : créer avec prototype `null`.

---

## 14. Questions ouvertes

### 14.1 Écarts volontaires entre [Réf] et [Amont-effectif] (décision de fidélité à confirmer)

| # | Sujet | [Réf] | [Amont-effectif] |
|---|---|---|---|
| F1 | Exécution M1 | exports lus, `update` appelé | `update` jamais trouvé, valeur remise à Null à chaque frame |
| F2 | Propriété `text` des textes | M2 seulement | M1 (premier niveau exécuté une 2ᵉ fois, valeur → Null, chaîne conservée) + M2 |
| F3 | Échec d'évaluation M1 | journalisé, valeur intacte | silencieux, valeur → Null |
| F4 | `update` non exporté | valeur intacte | valeur → Null |
| F5 | Retour vecteur `z`/`w` | lus | forcés à 0 |
| F6 | Retour objet non vectoriel | journal, ignoré | erreur fatale |
| F7 | `thisLayer` | couche du script appelé, accesseurs actifs | objet vide ; et lié au dernier script chargé |
| F8 | `getLayer` | objet couche fonctionnel | objet vide |
| F9 | `createScriptProperties` / `scriptProperties` | fonctionnels, défauts déclarés pris en compte, liés au bon module | comportement indéfini |
| F10 | `setTimeout`/`setInterval` | fonctionnels ; fonction d'annulation sans argument | comportement indéfini |
| F11 | Méthodes vecteurs | accessibles, sémantique WE | inaccessibles (exception) |
| F12 | Constructeurs `Vec2/3/4` | globaux, multi-arguments WE | non exposés |
| F13 | `WEMath`/`WEColor` | exports fonctionnels | import nommé → échec d'évaluation |
| F14 | `console.log` d'un objet non convertible | repli | indéfini |
| F15 | Délai de minuteur négatif | 0 | indéfini |

Recommandation : adopter [Réf]. Un « mode fidélité stricte » n'a d'intérêt que pour F1–F4 (effets visibles de la
remise à Null) ; tout le reste de [Amont-effectif] est indéfini ou plante.

### 14.2 Ambiguïtés et décisions à prendre

- **Q-M1-1** — `init(value)` / `destroy()` des scripts de propriété : non appelés (R3). Beaucoup de scripts WE
  initialisent leur état dans `init` ; recommandation : appeler `init(value)` une fois avant le premier `update`
  (résultat appliqué comme celui d'`update` selon [WE]) et `destroy()` à la fin. À trancher.
- **Q-M1-2** — Moment du premier `update` : au chargement, pendant la construction de l'objet (avant son
  initialisation graphique et avant la création des objets suivants). WE appelle plutôt `init` puis `update` à
  la première frame. Conserver ?
- **Q-M1-3** — `update` renvoyant `undefined` → remise à Null ([Amont-intention]). [WE] probable : valeur
  inchangée ou erreur. Recommandation : ignorer `undefined` (garder la valeur) si l'on accepte de s'écarter.
- **Q-M1-4** — Correction possible du §1.2 (script de `scale`/`angles`/`visible` posé sur la valeur générique non
  lue par le rendu image/texte) : reproduire (sans effet) ou appliquer le script aussi à la copie spécifique ?
  [Réf] actuelle : reproduire.
- **Q-M1-5** — Ordre des `update` : ordre lexicographique des clés `nom_id`. Conservé pour la fidélité ; WE suit
  probablement l'ordre des objets.
- **Q-RT-1** — File de jobs jamais exécutée : promesses et `async` inopérants. Recommandation : exécuter les jobs
  en attente une fois par frame après le tick JS (écart [WE]-compatible, invisible pour les scripts synchrones).
- **Q-RT-2** — Aucune limite d'exécution. Recommandation : un gestionnaire d'interruption (budget par appel, ex.
  quelques centaines de ms) journalisant et interrompant le script ; écart volontaire à valider.
- **Q-VEC-1** — Les inversions de l'intention amont (`subtract`, `divide`, `cross`, `mix`, `lengthSqr`, `dot`
  renvoyant un vecteur) ont été remplacées par la sémantique [WE]. Confirmer.
- **Q-VEC-2** — Méthodes binaires appelées avec une mauvaise arité : renvoient `undefined` (sauf `mix`/`equals`).
  Conservé.
- **Q-VEC-3** — `equals` : égalité exacte ; une variante avec tolérance 1e-5 existe dans l'amont mais n'est pas
  utilisée. Conservé exact.
- **Q-VEC-4** — Énumération des vecteurs vide (`JSON.stringify` → `"{}"`). [WE] inconnu. Conservé.
- **Q-COL-1** — `rgb2hsv`/`hsv2rgb` utilisent une teinte en **degrés** [0,360) ; la documentation WE ne précise
  pas l'échelle (0–1 possible). À vérifier sur des fonds réels.
- **Q-SC-1** — `thisScene.clearenabled` renvoie l'état du bloom et `skylightcolor` la couleur ambiante ;
  `bloomstrength`/`bloomthreshold` sont tronqués en entier. Reproduits (R1) ; corriger ?
- **Q-IN-1** — `input.cursorScreenPosition` est normalisé [0,1] dans l'espace UV de la scène, alors que [WE]
  parle de coordonnées écran (pixels probables). Reproduit (R1).
- **Q-TOD-1** — `engine.timeOfDay` ignore les secondes (résolution d'une minute). Reproduit.
- **Q-MED-1** — Événements média : `position`/`duration` en microsecondes (brut MPRIS) alors que [WE] utilise
  probablement des secondes ; couleurs de vignette codées en dur ; les 4 événements envoyés à chaque changement ;
  aucun envoi initial ; `mediaStatusChanged` absent. Reproduits.
- **Q-LS-1** — `localStorage` : en mémoire, `remove` au lieu de `delete`, valeurs converties en chaîne.
  Reproduit ; [WE] persiste et accepte tout type.
- **Q-M2-1** — Imitations M2 : `engine` ne fournit que `frametime` et `time` (pas `runtime`, pourtant très utilisé
  par les scripts WE → `NaN`) ; `thisScene` expose `time/currentTime/dt/fps` (non-WE). Reproduit (R1) ; envisager
  d'exposer aussi les vrais membres d'`engine` ?
- **Q-M2-2** — Un texte invisible ne fait pas avancer son script et `init` n'est appelé qu'au premier dessin
  visible. Reproduit.
- **Q-M2-3** — Les `scriptproperties` en M2 sont figées à la création (pas de suivi des propriétés utilisateur,
  sauf composants de vecteurs vivants). Reproduit.
- **Q-CON-1** — `console.log` concatène sans séparateur (WE/navigateurs : espace). Reproduit.
- **Q-ERR-1** — Valeur levée par les fonctions natives en cas d'erreur d'arité/type : non spécifiée ; proposer
  `TypeError` avec message explicite.

### 14.3 API SceneScript officielles non implémentées par l'amont (référence : documentation publique WE)

- **Globales absentes** : `thisObject` (propriété de script courante), `renderContext`.
- **`engine`** : `isRunningInEditor()`, `isPortrait()`, `isLandscape()`, `isDesktopDevice()`,
  `isMobileDevice()`, `isWallpaper()`, `isScreensaver()`, `registerAudioBuffers(résolution)` (→ `AudioBuffers`
  `left/right/average`), `registerAsset(fichier, précache)`, `screenResolution`, `canvasSize`, `userProperties`.
  `openUserShortcut` existe mais ne fait rien (WE : renvoie un booléen).
- **`input`** : `cursorWorldPosition` (toujours 0), `cursorLeftDown` (toujours faux) non implémentés.
- **`thisScene` (IScene)** : `getLayerByID`, `getLayerCount`, `enumerateLayers`, `destroyLayer`, `createLayer`,
  `sortLayer`, `getLayerIndex`, `getInitialLayerConfig`, `getCameraTransforms`, `setCameraTransforms`,
  `getAnimation`, `createModelData`, `destroyModelData` ; tous les membres en écriture.
- **Couches (ILayer et dérivés)** : écriture de toute propriété ; `name` ; `getTransformMatrix`,
  `rotateObjectSpace`, `lookAt`, `lookAtYaw`, `setParent`, `getParent`, `getChildren`, attachements ;
  image : `alignment`, animations de texture/vidéo/couches d'animation, os, blend shapes ; effets (`getEffect`,
  `getEffectCount`, `IEffect.setMaterialProperty`, …) ; texte : `font`, `padding`, alignements, `anchor`,
  limites de lignes/largeur, fond opaque ; son : `volume`, `play/stop/pause/isPlaying` ; particules : `instance`
  (et ses `controlpoint0..7`, `rate`, `count`…), `play/pause/stop/isPlaying/emitParticles` ; caméra : `fov`, `zoom`.
- **Événements** : `init`, `destroy` (M1), `resizeScreen`, `applyUserProperties`, `applyGeneralSettings`, les six
  événements curseur (`CursorEvent` : `worldPosition`, `localPosition`, `hitBox`), `mediaStatusChanged`
  (`enabled`).
- **Champs média manquants** : propriétés `subTitle`, `albumArtist`, `genres`, `contentType` ; vignette
  `textColor` ; couleurs de vignette réelles (extraction depuis la pochette).
- **Vecteurs** : constructeur depuis chaîne ; `distance`, `distanceSqr`, `isFinite`, `negate`, `reflect`,
  `refract` (Vec3), `perpendicular`, `angle`, `rotate` (Vec2), `project`, `angleBetween`, `toSpherical` et
  `Vec3.fromSpherical`, `clamp`, `fract`, `mod`, `step`, `smoothStep` ; arguments scalaires/vecteurs mixtes pour
  `mix`.
- **Matrices** : `Mat3`, `Mat4` entièrement absents.
- **Modules** : `WEVector` (`angleVector2`, `vectorAngle2`) absent.
- **`localStorage`** : `delete`, paramètre `location`, `LOCATION_GLOBAL`, `LOCATION_SCREEN`, persistance,
  quota (100 Ko par fond d'écran selon WE).
- **Propriétés de script** : types de contrôle éventuels au-delà des cinq `add*` connus (fichier, répertoire,
  saisie de texte…) — non vérifiés dans la doc publique.
- **Portée des scripts** : scripts sur constantes de shader, visibilité d'effets, couches d'animation, sons,
  caméra, propriétés de texte autres que celles du §1.2, `pointsize` en minuscules.
