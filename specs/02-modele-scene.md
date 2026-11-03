# Spécification 02 — Modèle de données : projet, scène, objets, matériaux, effets

> Document de salle blanche. Il décrit le **comportement observable** de l'implémentation de
> référence (formats lus, valeurs par défaut, conversions, cas limites) avec des mots propres.
> Aucun code, commentaire ni identifiant interne de l'implémentation de référence n'est repris.
> Les noms de clés JSON, chaînes magiques, noms de fichiers d'assets Wallpaper Engine (WE),
> noms de cibles de rendu et constantes numériques sont des faits imposés par le format.
>
> Les implémenteurs n'ont **pas** accès au code de référence : tout ce qui est nécessaire doit
> se trouver ici ou dans les autres spécifications citées.

---

## 0. Périmètre, vocabulaire, règles générales

### 0.1 Périmètre

Inclus :

- `project.json` (types `scene` / `video` / `web`), propriétés utilisateur, surcharges en ligne
  de commande ;
- `scene.json` (`camera`, `general`, `objects`) ;
- tous les types d'objets de scène reconnus et leurs clés ; fichiers de particules ;
- `models/*.json`, `materials/*.json`, `effects/*.json` ;
- valeurs dynamiques, liaisons aux propriétés utilisateur, conditions, scripts attachés (données
  seulement) ;
- ordre de construction, ordre de rendu, héritage/précédence des valeurs (textures, combos,
  constantes, transformations, visibilité) ;
- l'objet « bloom » synthétique ;
- sorties texte de diagnostic (liste des propriétés, dump de structure) ;
- annexes binaires PKG et TEX (résumé autosuffisant ; une autre spec peut les détailler).

Exclus (renvoyés à d'autres specs) : rendu GPU détaillé, préprocesseur/compilateur de shaders
(sauf les règles de précédence décrites §12), moteur de script, audio, vidéo, web, montage du
système de fichiers virtuel (VFS).

### 0.2 Vocabulaire employé dans ce document

| Terme | Sens |
|---|---|
| **Valeur** | Conteneur « multi-vues » : une étiquette de type + une projection simultanée en flottant, entier, booléen, vec2, vec3, vec4 et texte (§4). |
| **Réglage** | Une Valeur + éventuellement une Propriété liée + éventuellement une condition + éventuellement un script (§5). C'est la forme de presque tous les champs « animables » de la scène. |
| **Propriété** | Paramètre utilisateur déclaré dans `project.json` → `general.properties` (§3). |
| **Chemin d'asset** | Chemin relatif à la racine du VFS, séparateur `/`, utilisé tel quel (§1.2). |
| **E1** | Erreur fatale de chargement : le fond d'écran ne se charge pas (l'erreur remonte jusqu'à l'application). |
| **E2** | Dans l'implémentation de référence, terminaison brutale du processus (erreur de type levée dans un contexte qui l'interdit). **Recommandation** : traiter comme E1. Voir Q12. |
| **W** | Avertissement journalisé ; la donnée fautive est ignorée ou remplacée par un défaut. |

### 0.3 Règles JSON générales

- JSON strict : pas de commentaires, pas de virgules finales, encodage UTF‑8. Une erreur de
  syntaxe dans un fichier lu est **E1** (sauf mention contraire, ex. `.tex-json` §B.6).
  Les exemples de ce document sont en `jsonc` avec des commentaires `//` **uniquement** pour
  l'explication.
- Les clés sont sensibles à la casse.
- Clé dupliquée dans un objet : la **dernière** occurrence l'emporte.
- Itération sur un objet JSON (propriétés, combos, constantes, scriptproperties…) : ordre
  **lexicographique des clés** (comparaison octet par octet). L'ordre des tableaux est conservé.
- Un nombre JSON est « entier » s'il est écrit sans point décimal ni exposant (`1`, `-3`), sinon
  « flottant » (`1.0`, `1e3`). Cette distinction a des effets (§5.2).
- `null` est traité comme **absent** pour les lectures optionnelles (modes O et L, §0.4). Pour les
  lectures requises, `null` compte comme présent.

### 0.4 Modes de lecture d'un champ

Chaque champ des tableaux de ce document est annoté par un mode :

| Mode | Comportement si absent ou `null` | Comportement si présent |
|---|---|---|
| **R** (requis typé) | absent → E1 ; `null` → E1 (conversion impossible) | conversion stricte (§0.5) ; échec → E1 |
| **O** (optionnel typé) | valeur par défaut indiquée | conversion stricte ; échec → **E2** |
| **L** (réglage, avec défaut) | Réglage **constant** contenant le défaut indiqué, non lié | analyse §5 ; erreurs → E1 |
| **L!** (réglage requis) | absent → E1 ; `null` → Réglage de type Nul | analyse §5 |
| **Lc / L!c** | comme L / L! mais « couleur attendue » (§5.2) | |
| **T** (tolérant) | défaut | si le type JSON n'est pas celui attendu → défaut, sans erreur |

### 0.5 Conversions strictes

| Cible | JSON accepté | Détail |
|---|---|---|
| flottant | nombre (entier ou flottant), booléen (`true`=1, `false`=0) | chaîne/null/tableau/objet → échec |
| entier 32 bits signé / non signé | idem | flottant → troncature vers zéro ; entier hors plage → réduit modulo 2³² (complément à deux) |
| booléen | booléen **uniquement** | `0`/`1` → échec |
| chaîne | chaîne uniquement | |
| vecN strict | nombre → les N composantes valent ce nombre ; chaîne → §0.6 avec contrôle du nombre de composantes | autre type → échec |
| couleur stricte | chaîne → §0.7 | autre type → échec |

### 0.6 Chaînes vectorielles (`"x y z"`)

Wallpaper Engine écrit les vecteurs sous forme de chaîne de nombres séparés par des espaces.
Règles exactes :

1. **Séparateur** : le caractère espace U+0020 **seul**. Les tabulations ne séparent pas.
2. **Comptage** : on repère au plus les trois premiers espaces. Nombre de composantes
   `k` = 1 s'il n'y a aucun espace, 2 s'il y en a un, 3 s'il y en a deux, 4 s'il y en a trois ou
   plus.
3. **Début des composantes** : la composante 0 commence au début de la chaîne ; la composante i
   (i ≥ 1) commence juste après le i‑ème espace.
4. **Lecture d'une composante flottante** : on ignore les blancs initiaux, puis on lit le plus long
   préfixe formant un nombre flottant valide (décimal, exposant, `inf`, `nan`, hexadécimal
   flottant) ; la lecture s'arrête au premier caractère invalide (donc à l'espace suivant). Aucun
   préfixe valide → 0.
5. **Lecture d'une composante entière** (uniquement chemin entier des couleurs, §0.7) : blancs
   initiaux ignorés, signe optionnel, chiffres décimaux ; aucun chiffre → échec ; dépassement de
   la plage 32 bits → échec.
6. **Contrôle strict du nombre de composantes** (lectures R/O vecteur) : vec2 exige exactement
   1 espace ; vec3 exactement 2 ; vec4 au moins 3 (les composantes au-delà de la 4ᵉ sont
   ignorées). Violation → échec.
7. En lecture **Réglage** (§5.2), le nombre de composantes n'est pas contrôlé : il détermine le
   type.

Conséquences (comportement de référence, voir Q4) : `"1  2"` (deux espaces) compte 3 composantes
et donne (1, 2, 2) ; `" 1 2"` (espace initial) compte 3 composantes et donne (1, 1, 2) ;
`"3 "` (espace final) compte 2 composantes et donne (3, 0).

### 0.7 Chaînes de couleur

Utilisé pour les Réglages « couleur attendue » (Lc), les propriétés de type `color` et les mises à
jour textuelles de ces propriétés. Toujours avec alpha implicite = 1.

1. Toutes les virgules sont remplacées par des espaces (sans fusionner les espaces).
2. **Notation hexadécimale** (premier caractère `#`) — reste = chiffres hexadécimaux :
   - 3 chiffres `RGB` → chaque chiffre doublé + `ff` → 8 chiffres ;
   - 4 chiffres `RGBA` → chaque chiffre doublé → 8 chiffres ;
   - 6 ou 8 chiffres → tels quels ; autre longueur → E1.
   - Le texte obtenu est converti en **entier signé 32 bits** en base 16 (dépassement → E1), puis
     R = bits 24–31, G = bits 16–23, B = bits 8–15, A = bits 0–7, chacun divisé par 255.
   - Effets de bord de référence (Q2) : 6 chiffres `#RRGGBB` donnent R=0, G=RR, B=GG, A=BB ;
     8 chiffres (y compris issus de 3 ou 4) dont le premier octet ≥ 0x80 provoquent E1.
3. Sinon, `k` = nombre de composantes (§0.6) ; `k` ∉ {3, 4} → E1.
4. Si la chaîne **ne contient aucun `.`** : chemin **entier** — composantes lues comme entiers
   (§0.6‑5), alpha = 255 si k = 3 ; chaque canal divisé par 255. Ex. `"255 128 0"` →
   (1, 0.502, 0, 1). **Attention** : `"1 1 1"` → (1/255, 1/255, 1/255, 1) (Q1).
5. Sinon chemin **flottant** : composantes flottantes, alpha = 1 si k = 3.
6. Aucun bornage (clamp) n'est appliqué.

Constantes : **blanc** = (1, 1, 1, 1), **noir** = (0, 0, 0, 1).

---

## 1. Chaîne de chargement

### 1.1 Séquence

1. Montage du VFS pour le dossier du fond d'écran (spec dédiée). Des fichiers virtuels sont
   ajoutés pour l'effet bloom et la copie de passes (§14).
2. Lecture de `project.json` à la racine du VFS ; erreur de lecture ou de syntaxe → E1.
3. Analyse du projet (§2), **propriétés comprises** (§3). Les propriétés existent donc avant la
   scène, ce qui permet aux Réglages de s'y lier.
4. Analyse du fichier principal selon le type (§2.3). Pour une scène : `scene.json` (§6), puis
   chaque objet dans l'ordre du tableau (§7). Chaque modèle, matériau, effet et fichier de
   particules référencé est lu et analysé **à chaque référence, sans cache** : deux images
   utilisant le même modèle possèdent des copies indépendantes (une modification de l'une, ex.
   textures d'instance §12.1, n'affecte pas l'autre).
5. Application des surcharges de propriétés de la ligne de commande (§3.5) à **chaque** projet
   chargé ; les Réglages liés sont mis à jour immédiatement (§4.4).
6. Si l'option de listage des propriétés est active : affichage (§15.1) puis fin du programme
   (code 0) sans rendu.
7. Construction de la scène de rendu (§13), qui lit les Réglages **après** surcharges.

Les champs « figés à l'analyse » (marqués *instantané* dans les tableaux) sont lus à l'étape 4,
donc **avant** les surcharges de l'étape 5 (Q18).

### 1.2 Chemins d'assets

- Tous les chemins trouvés dans les JSON (`file`, `image`, `material`, `particle`, `sound`,
  `font`, `puppet`, …) sont relatifs à la racine du VFS et utilisés tels quels.
- Noms de textures (§11.1) : un nom `N` désigne le fichier `materials/N.tex` (et ses
  métadonnées optionnelles `materials/N.tex-json`), **sauf** si `N` commence par `_rt_` ou
  `_alias_` : c'est alors le nom d'une cible de rendu (FBO).
- Noms de shaders : `shaders/<nom>.vert` / `shaders/<nom>.frag` (spec shaders).

---

## 2. `project.json`

### 2.1 Schéma

| Clé | Mode | Type / défaut | Notes |
|---|---|---|---|
| `type` | R | chaîne | Comparée **sans tenir compte de la casse ASCII** : `scene`, `video`, `web`. Autre → E1. |
| `title` | R | chaîne | Titre. |
| `file` | R | chaîne | Fichier principal (scène : `scene.json` en général ; vidéo : fichier vidéo ; web : page HTML). Non‑chaîne → E1. |
| `workshopid` | facultatif | nombre ou chaîne | Voir §2.2. |
| `general` | facultatif | objet | Absent/`null` → pas de propriétés, audio désactivé. |
| `general.supportsaudioprocessing` | O | booléen, `false` | Le fond d'écran demande l'analyse audio (spec audio). |
| `general.properties` | facultatif | objet | Propriétés utilisateur (§3). |

Toutes les autres clés (`preview`, `description`, `tags`, `visibility`, `contentrating`,
`version`, `general.localization`, etc.) sont ignorées.

### 2.2 Identifiant de projet

- Un compteur global, initialisé à 0, est **décrémenté à chaque analyse de projet**, que
  `workshopid` soit présent ou non. L'identifiant par défaut est la représentation décimale du
  compteur après décrément : `-1` pour le premier projet analysé, `-2` pour le second, etc.
- `workshopid` nombre → converti en **entier signé 32 bits** puis en texte décimal (les
  identifiants > 2 147 483 647 sont donc altérés, Q7).
- `workshopid` chaîne → utilisée telle quelle.
- Autre type → W, l'identifiant par défaut est conservé.
- Usage connu : construction du schéma d'URL interne des fonds d'écran web (spec web).

### 2.3 Fichier principal selon le type

| Type | Contenu du modèle |
|---|---|
| `scene` | `file` est lu, analysé comme `scene.json` (§6). |
| `video` | Le chemin `file` est seulement mémorisé. |
| `web` | Le chemin `file` est seulement mémorisé. |

### 2.4 Exemple minimal

```jsonc
{
  "type": "Scene",                 // insensible à la casse
  "title": "Mon fond",
  "file": "scene.json",
  "workshopid": "123456789",       // préférer la forme chaîne (Q7)
  "general": {
    "supportsaudioprocessing": true,
    "properties": {
      "schemecolor": { "type": "color", "text": "ui_browse_properties_scheme_color",
                       "value": "0.2 0.4 0.8" },
      "showclock":   { "type": "bool", "text": "Horloge", "value": true },
      "speed":       { "type": "slider", "text": "Vitesse", "value": 1.5,
                       "min": 0, "max": 10, "step": 0.1 },
      "mode":        { "type": "combo", "text": "Mode", "value": 1,
                       "options": [ { "label": "Calme", "value": 0 },
                                    { "label": "Agité", "value": 1 } ] },
      "titre":       { "type": "group" }        // ignoré silencieusement
    }
  }
}
```

---

## 3. Propriétés utilisateur

### 3.1 Déclaration

`general.properties` est un objet : **clé = nom de la propriété** (sensible à la casse), valeur =
objet de définition. L'ensemble des propriétés est stocké trié par nom (§0.3).

Clés communes de la définition :

| Clé | Mode | Défaut | Notes |
|---|---|---|---|
| `type` | — | absent | Détermine le genre (§3.2). Absent ou `group` → propriété ignorée silencieusement. |
| `text` | O chaîne | `""` | Libellé (souvent une clé de localisation WE). |
| `value` | selon type | | §3.2. |
| `order`, `condition`, `index`, `precision`, `fraction`, … | — | | Ignorées (métadonnées d'interface WE). |

Une définition qui n'est pas un objet est ignorée.

### 3.2 Genres de propriétés

| `type` | Stockage | Lecture de `value` | Étiquette initiale (§4.1) | Autres clés |
|---|---|---|---|---|
| `color` | couleur | **R chaîne** → §0.7 | Vec4 | — |
| `bool` | booléen | **O booléen**, défaut `false` | Booléen | — |
| `slider` | flottant | **R flottant** (nombre ou booléen) | Flottant | `min`, `max`, `step` : O flottant, défaut 0 chacun (mémorisés, aucun bornage appliqué) |
| `combo` | texte | **R** chaîne ou nombre ; nombre → converti en entier (troncature) puis en texte décimal | Texte (ou Nul, voir ci‑dessous) | `options` R tableau |
| `text` | aucun | non lue | Nul | Propriété purement décorative |
| `scenetexture` | texte | **R chaîne** | Texte | — |
| `file`, `directory` | texte | **O chaîne**, défaut `""` | Texte | — |
| `textinput`, `usershortcut` | texte | **R** JSON quelconque, stocké sous forme de **sérialisation JSON compacte** (une chaîne `abc` devient le texte `"abc"` avec guillemets ; un objet devient `{"k":1}`) | Texte | Q9 |
| `group` ou absent | — | — | — | ignorée |
| autre | — | — | — | W, ignorée |

**Options d'un combo** (`options`) :

- Doit être un tableau (sinon E1). Chaque élément qui n'est pas un objet est ignoré.
- Pour chaque option : `value` R (chaîne, ou nombre converti en entier puis en texte décimal ;
  autre type → E1) ; `label` R chaîne.
- Table « texte de valeur → libellé », triée par texte de valeur ; en cas de doublon, la
  première option rencontrée est conservée.
- Si la valeur initiale n'est pas une des valeurs d'options : W, et la propriété reste de type
  **Nul** (toutes vues à zéro, voir §4).

### 3.3 Mise à jour d'une propriété à partir d'un texte

Utilisé par les surcharges de la ligne de commande (§3.5) ; source de mise à jour = « utilisateur ».

| Genre | Interprétation du texte |
|---|---|
| `slider` | Lecture d'un flottant sur le préfixe du texte (blancs initiaux ignorés, caractères suivants ignorés : `"0.5abc"` → 0.5). Aucun nombre → erreur non rattrapée au démarrage (Q13). Étiquette Flottant. |
| `bool` | `"true"` ou `"1"` → vrai ; **tout autre texte** → faux (y compris `"TRUE"`, `"yes"`). |
| `color` | §0.7 (erreur → fatale au démarrage). |
| `combo` | Si le texte est exactement une des valeurs d'options → stocké (Texte) ; sinon W et **aucun changement**. |
| `text` | Le texte est stocké (étiquette Texte) ; l'affichage (§15.1) montre toujours le libellé. |
| `scenetexture`, `file`, `directory`, `textinput`, `usershortcut` | Texte brut stocké tel quel (sans sérialisation JSON, contrairement à l'initialisation de `textinput`). |

### 3.4 Propagation

Toute mise à jour d'une propriété est propagée **synchroniquement** à tous les Réglages liés
(§4.4, §5.3), quel que soit le nombre de Réglages liés.

### 3.5 Surcharges en ligne de commande

- Option répétable `--set-property <arg>` (alias `--property`).
- `<arg>` est découpé au **premier** `=` : nom = partie gauche, valeur = partie droite (peut être
  vide, peut contenir d'autres `=`). Sans `=` : valeur `"1"`.
- Si le même nom apparaît plusieurs fois, la dernière occurrence gagne.
- Application après chargement complet de chaque projet, en parcourant les propriétés **du
  projet** par ordre alphabétique ; pour chacune dont le nom figure dans les surcharges : un
  message informatif indiquant le nom est journalisé, puis mise à jour §3.3. Les noms inconnus du
  projet sont ignorés silencieusement.
- Les surcharges sont réappliquées à chaque nouveau projet chargé (multi‑écrans, listes de
  lecture).
- Option `--list-properties` (alias `-l`) : après application des surcharges, chaque propriété
  est affichée (§15.1) ; le programme se termine ensuite (code 0).

---

## 4. Valeur dynamique (modèle multi‑vues)

### 4.1 Étiquettes

Nul, Flottant, Entier, Booléen, Vec2, Vec3, Vec4, Texte. Une Valeur neuve est Nul (toutes vues à
zéro/faux, texte vide). L'étiquette Nul a le code numérique 0 (visible uniquement dans un message
§4.3).

### 4.2 Table de projection

Lorsqu'une Valeur reçoit une donnée, **toutes** les vues sont recalculées ainsi
(`trunc` = conversion en entier 32 bits par troncature vers zéro ; `s(x)` = vecteur dont toutes
les composantes valent x) :

| Donnée reçue | Étiquette | flottant | entier | booléen | vec2 | vec3 | vec4 | texte |
|---|---|---|---|---|---|---|---|---|
| flottant f | Flottant | f | trunc(f) | trunc(f) ≠ 0 | s(f) | s(f) | s(f) | "" |
| entier i | Entier | i | i | i ≠ 0 | s(i) | s(i) | s(i) | "" |
| booléen b | Booléen | b?1:0 | b?1:0 | b | s(b?1:0) | s(…) | s(…) | "" |
| vec2 (x,y) | Vec2 | x | trunc(x) | x ≠ 0 | (x,y) | (x,y,0) | (x,y,0,0) | "" |
| vec3 (x,y,z) | Vec3 | x | trunc(x) | x ≠ 0 | (x,y) | (x,y,z) | (x,y,z,0) | "" |
| vec4 (x,y,z,w) | Vec4 | x | trunc(x) | x ≠ 0 | (x,y) | (x,y,z) | (x,y,z,w) | "" |
| couleur (r,g,b,a) | **Vec4** | r | trunc(255·r) | **a ≠ 0** | (r,g) | (r,g,b) | (r,g,b,a) | "" |
| texte t, sans condition | Texte | 0 | 0 | faux | 0 | 0 | 0 | t |
| texte t, avec condition c | **Booléen** | t==c?1:0 | t==c?1:0 | t==c | s(t==c?1:0) | s(…) | s(…) | t |
| nul | Nul | 0 | 0 | faux | 0 | 0 | 0 | *inchangé* (Q23) |

Remarques normatives :

- Un flottant 0.5 a une vue booléenne **fausse** (troncature), alors qu'un vec2 (0.5, 0) a une vue
  booléenne vraie (Q24).
- La comparaison texte/condition est une égalité exacte d'octets.

### 4.3 Représentation textuelle (affichage)

| Étiquette | Format |
|---|---|
| Flottant | notation fixe à 6 décimales (`1.500000`) |
| Entier | décimal |
| Booléen | `1` ou `0` |
| Vec2/3/4 | composantes à 6 décimales séparées par `", "` (`1.000000, 0.000000, 0.000000`) |
| Texte | texte brut |
| Nul | message indiquant une conversion inconnue suivi du code de type `0` |

### 4.4 Liaison (copie et abonnement)

Une Valeur V peut être **liée** à une Valeur source P (en pratique : une Propriété) :

1. Copie immédiate : si P est Nul → V reçoit « nul » ; sinon V reçoit **toutes** les vues et
   l'étiquette de P, puis, si V porte une condition c **et** que l'étiquette de P est Texte, les
   vues de V sont recalculées comme la ligne « texte t avec condition c » (t = texte de P).
2. Abonnement : chaque mise à jour ultérieure de P répète l'étape 1, avec la même source de mise
   à jour.
3. Une Valeur peut avoir plusieurs abonnés ; une mise à jour d'un abonné (ex. par un script) ne
   remonte jamais vers P. La prochaine mise à jour de P écrase la valeur de l'abonné.

Les sources de mise à jour sont : initialisation, script, utilisateur. Elles sont transmises aux
abonnés mais n'altèrent pas la sémantique des vues.

---

## 5. Réglages (valeurs liables)

### 5.1 Formes JSON acceptées

```jsonc
"alpha": 0.5                                   // scalaire
"origin": "960 540 0"                          // chaîne vectorielle
"visible": { "value": true }                   // objet minimal
"color":   { "value": "1 0 0", "user": "schemecolor" }           // lié à une propriété
"visible": { "value": false,
             "user": { "name": "mode", "condition": "1" } }      // lié + condition
"text":    { "value": "00:00",
             "script": "export function update(v) { return v; }",
             "scriptproperties": { "format": { "value": 24, "user": "fmt" } } }
```

### 5.2 Algorithme d'analyse

Entrée : nœud JSON `J`, drapeau « couleur attendue » (vrai pour les champs marqués Lc/L!c).

1. **Décomposition** — Si `J` est un objet : la valeur brute `B` = `J.value` (**obligatoire**, même
   si `user` est présent ; absent → E1). Si `J.script` est présent et non nul, c'est le source
   du script (chaîne, sinon E1) et `J.scriptproperties` (facultatif) est retenu. Si `J.user` est
   présent et non nul, il est retenu. Si `J` n'est pas un objet : `B` = `J`, sans `user` ni
   script.
2. **Valeur initiale** à partir de `B` :
   - chaîne et couleur attendue → §0.7 → donnée couleur ;
   - chaîne sinon → `k` = nombre de composantes (§0.6) :
     - `k` = 1 : tentative de lecture du texte entier comme flottant (blancs initiaux tolérés ;
       **tout** le reste doit être consommé). Succès → flottant ; sinon (y compris chaîne vide)
       → texte brut. Ex. `"0.5"` → Flottant ; `"5px"` → Texte ; `"inf"` → Flottant ;
     - `k` = 2 → vec2 ; `k` = 3 → vec3 ; `k` ≥ 4 → vec4 (4 premières composantes) ;
   - nombre entier JSON → entier (32 bits, troncature/réduction) ;
   - nombre flottant JSON → flottant ;
   - booléen → booléen ;
   - `null`, tableau, objet → **Nul**.
3. **Script** — s'il y a un source de script : il est attaché à la Valeur. Si `scriptproperties`
   est un objet, chacune de ses entrées est analysée par ce même algorithme (couleur attendue =
   faux) et attachée sous son nom. L'exécution relève de la spec scripts.
4. **Liaison** — si `user` est retenu :
   - chaîne → nom de propriété = cette chaîne, pas de condition ;
   - objet → nom = `user.name` (R chaîne), condition = `user.condition` (R chaîne ; un nombre
     → E1, Q30) ;
   - autre type → E1.
   On cherche la propriété de ce nom (égalité exacte) dans le projet. **Trouvée** : la condition
   éventuelle est attachée à la Valeur, puis la Valeur est liée (§4.4) — la valeur initiale de
   l'étape 2 est donc immédiatement écrasée. **Non trouvée** : aucune erreur, la Valeur garde sa
   valeur initiale et ne porte pas de condition.
5. Le Réglage résultant mémorise : la Valeur, la propriété liée (si trouvée), la condition (si
   déclarée).

### 5.3 Conditions de visibilité (et autres)

- Une condition ne transforme la valeur que si la propriété liée est d'étiquette **Texte**
  (combo, textinput, fichier, scenetexture…). Résultat : Booléen (vrai si texte de la propriété
  == condition).
- Pour une propriété booléenne, curseur ou couleur, la condition est **ignorée** et la valeur de
  la propriété est copiée telle quelle.
- Comme une valeur de combo est stockée en Texte, un champ numérique lié à un combo **sans
  condition** voit 0 / faux / vecteur nul (Q8).
- La condition est réévaluée à chaque changement de la propriété (abonnement §4.4).

Exemple :

```jsonc
// project.json : "mode": { "type": "combo", "value": 1, "options": [...0..., ...1...] }
// scene.json, objet image :
"visible": { "value": true, "user": { "name": "mode", "condition": "1" } }
// mode = "1" → visible = Booléen vrai ; --set-property mode=0 → visible = faux
```

### 5.4 Réglages par défaut (champ absent)

Mode L/Lc : un Réglage non lié dont la Valeur est la constante par défaut, avec l'étiquette
correspondant au type de la constante par défaut indiquée dans les tableaux : booléen → Booléen, flottant → Flottant, entier → Entier,
vecN → VecN, couleur → Vec4 (projection « couleur »).

---

## 6. `scene.json`

### 6.1 Racine

| Clé | Mode | Notes |
|---|---|---|
| `camera` | R objet | §6.2 |
| `general` | R objet | §6.3 |
| `objects` | R | Tableau d'objets (§7). Si c'est un objet JSON, ses valeurs sont parcourues dans l'ordre des clés. |

Les autres clés sont ignorées.

### 6.2 `camera`

| Clé | Mode | Défaut | Notes |
|---|---|---|---|
| `center` | R vec3 strict | — | Point visé. |
| `eye` | R vec3 strict | — | Position. |
| `up` | R vec3 strict | — | Vecteur haut. |
| `nearz` | L | 0.0 | Plan proche. Lu dans `camera` (et **non** dans `general`, Q5). |
| `farz` | L | 1000.0 | Plan lointain. Idem. |
| `fov` | L | 50.0 | Champ de vision. Idem. |

### 6.3 `general`

| Clé | Mode | Défaut | Usage (pour orientation) |
|---|---|---|---|
| `ambientcolor` | L | vec3(0) | Couleur ambiante (exposée aux scripts). |
| `skylightcolor` | L | vec3(0) | Couleur du ciel. |
| `clearcolor` | L | vec3(1) | Couleur d'effacement : vue vec3 lue **une fois** à la construction de la scène, alpha forcé à 1. |
| `camerafade` | L | faux | |
| `camerapreview` | O booléen | faux | |
| `bloom` | L | faux | Vue booléenne lue une fois à la construction : crée l'objet bloom synthétique (§14). |
| `bloomstrength` | L | 0.0 | Vue flottante, instantané à la construction (§14). |
| `bloomthreshold` | L | 0.0 | Idem. |
| `cameraparallax` | L | faux | Lu à chaque image. |
| `cameraparallaxamount` | L | 1.0 | Lu à chaque image. |
| `cameraparallaxdelay` | L | 0.0 | Lu à chaque image. |
| `cameraparallaxmouseinfluence` | L | 1.0 | Lu à chaque image. |
| `camerashake` | L | faux | Exposé aux scripts. |
| `camerashakeamplitude` | L | 0.0 | |
| `camerashakeroughness` | L | 0.0 | |
| `camerashakespeed` | L | 0.0 | |
| `orthogonalprojection` | R | — | Objet (voir ci‑dessous). Valeur `null` → E1 (Q6). |

Formule de parallaxe (pour mémoire ; détail dans la spec caméra) : facteur
`d = clamp(cameraparallaxdelay × Δt, 0, 1)` ; déplacement ← `mix(déplacement,
(souris − (0.5, 0.5)) × amount × mouseinfluence, d)`, souris normalisée dans [0, 1]².

`orthogonalprojection` :

| Clé | Mode | Notes |
|---|---|---|
| `auto` | O booléen, faux | |
| `width` | R entier si `auto` faux | Si `auto` vrai : non lu, vaut 0. |
| `height` | R entier si `auto` faux | Idem. |

Projection automatique (à la construction de la scène) : pour chaque objet **image** du modèle,
avec `origine` = vue vec3 de son `origin` et `taille` = sa taille instantanée (§7.3) :
`ex = max(ex, |origine.x| + taille.x/2)`, `ey = max(ey, |origine.y| + taille.y/2)`. Si `ex > 0` et
`ey > 0` : largeur = 2·ex, hauteur = 2·ey ; sinon : résolution totale de la sortie.

Clés WE ignorées : `clearenabled`, `hdr`, `zoom`, `perspectiveoverridefov`, `lightconfig`,
`gravity`, `wind…`, etc.

### 6.4 Exemple minimal

```jsonc
{
  "camera": { "center": "0.00000 0.00000 -1.00000",
              "eye": "0.00000 0.00000 0.00000",
              "up": "0.00000 1.00000 0.00000" },
  "general": {
    "clearcolor": { "value": "0 0 0", "user": "schemecolor" },   // lié (vec3 ← couleur)
    "bloom": true, "bloomstrength": 2.0, "bloomthreshold": 0.65,
    "cameraparallax": true, "cameraparallaxamount": 0.5,
    "orthogonalprojection": { "width": 1920, "height": 1080 }
  },
  "objects": [ /* §7 */ ]
}
```

---

## 7. Objets de scène

### 7.1 Classification (premier critère satisfait)

1. `image` présent **et chaîne** → **Image** (§7.3).
2. sinon `sound` présent **et tableau** → **Son** (§7.6).
3. sinon `particle` présent et (objet, ou chaîne non vide différente de `"None"`) →
   **Particule** (§7.8).
4. sinon `text` présent (n'importe quelle valeur, même `null`) → **Texte** (§7.7).
5. sinon `light` présent → objet **générique** + W (lumières non prises en charge).
6. sinon `shape` présent → objet **générique** + W (lumières volumiques non prises en charge).
7. sinon → objet **générique** ; si `solid` (O booléen, défaut faux) n'est pas vrai, W
   (« type inconnu ») avec le contenu de l'objet.

Les objets génériques servent de groupes / nœuds de transformation (parents) et sont exposés aux
scripts. Les objets « modèle 3D » de WE tombent dans ce cas (Q20).

### 7.2 Champs communs (tous types)

| Clé | Mode | Défaut | Notes |
|---|---|---|---|
| `id` | R entier | — | Identifiant unique attendu (Q17). |
| `name` | R chaîne | — | |
| `dependencies` | facultatif | `[]` | Si tableau : chaque élément converti strictement en entier (échec → E1). Sinon ignoré. |
| `parent` | O entier | aucun | Identifiant du parent. |
| `origin` | L | vec3(0) | Position (unique source d'origine, pour tous les types). |
| `scale` | L | vec3(1) | Échelle « de groupe » (utilisée pour les objets génériques). |
| `angles` | L | vec3(0) | Angles « de groupe » (radians ; seule la composante z sert aux transformations). |
| `visible` | L | vrai | Visibilité « de groupe » (seulement exposée aux scripts). |

**Mode dégradé** : si la lecture de ces champs échoue (ex. `name` numérique, `id` absent), W puis
relecture : `id` = valeur numérique tronquée si `id` est un nombre, sinon −1 ; `name` = la chaîne,
ou le décimal de l'entier si c'est un nombre, sinon `"unknown"` ; les autres champs sont relus
normalement (un second échec → E1).

Pour les images et particules, `scale`/`angles`/`visible` sont **relus** par le type ; pour les
textes, `scale`/`visible` (Réglages distincts mais liés à la même propriété le cas échéant). Les
transformations et la visibilité de ces types utilisent les Réglages relus ; ceux « de groupe »
ne sont alors pas utilisés par le rendu (l'exposition aux scripts relève de la spec scripts).

### 7.3 Image

| Clé | Mode | Défaut | Notes |
|---|---|---|---|
| `image` | R chaîne | — | Chemin du modèle `models/*.json` (§8) ; chargement immédiat, échec → E1. |
| `scale` | L | vec3(1) | |
| `angles` | L | vec3(0) | Seule `z` est utilisée (rotation dans le plan). |
| `visible` | L | vrai | Vue booléenne. |
| `alpha` | L | 1.0 | |
| `color` | Lc | blanc | Teinte (Vec4). |
| `horizontalalign` | O chaîne | voir note | Si absent : `alignment` (O chaîne), sinon `"center"`. |
| `size` | L, **instantané** | vec2(0) | Seule la vue vec2 est conservée, au moment de l'analyse. (0,0) = taille déduite de la texture. |
| `parallaxDepth` | L | vec2(0) | Attention à la casse (camelCase). |
| `colorBlendMode` | L | Entier 0 | > 0 : passe de mélange supplémentaire avec combo `BLENDMODE` = vue entière (§12.6). |
| `brightness` | L | 1.0 | |
| `effects` | facultatif | `[]` | Tableau d'effets (§7.4) ; non‑tableau → aucun. |
| `animationlayers` | facultatif | `[]` | Tableau (§7.5) ; non‑tableau → aucun. |
| `instance` | facultatif | — | Objet : `textures`, `usertextures` (cartes de textures §11.1) fusionnées dans la **première passe** du matériau du modèle (§12.1). Ignoré si non objet ou si le matériau n'a aucune passe. |

Alignement (consommé par le rendu) : la chaîne est examinée par recherche de sous‑chaînes
`top`, `bottom`, `left`, `right` (ex. `"topleft"`). Le point d'ancrage `origin` est le centre
par défaut.

Clés WE ignorées : `copybackground`, `ledsource`, `perspective`, `locktransforms`, `solid` (sauf
§7.1), `autosize` (au niveau objet), etc.

### 7.4 Effets d'une image

Chaque élément de `effects` :

| Clé | Mode | Défaut | Notes |
|---|---|---|---|
| `file` | R chaîne | — | Chemin `effects/…/effect.json` (§10), chargé immédiatement ; échec → E1. |
| `id` | O entier | −1 | |
| `name` | O chaîne | libellé générique | Sans effet observable. |
| `visible` | L | vrai | Évaluée **une seule fois** à la mise en place de l'image (§13.4). |
| `passes` | facultatif | `[]` | Tableau de **surcharges de passe** (ci‑dessous) ; non‑tableau → aucune. |

Surcharge de passe (élément de `passes`) :

| Clé | Mode | Défaut | Notes |
|---|---|---|---|
| `id` | O entier | −1 | Non utilisé pour l'appariement (§12.2). |
| `combos` | facultatif | `{}` | Objet nom → entier strict (§11.2). Non‑objet → vide. |
| `constantshadervalues` | facultatif | `{}` | Objet nom → Réglage (§11.3). Non‑objet → vide. |
| `textures` | facultatif | `{}` | Carte de textures (§11.1). |
| `usertextures` | facultatif | `{}` | Carte de textures. |

Exemple :

```jsonc
{
  "id": 12, "name": "Fond", "image": "models/fond.json",
  "origin": "960 540 0", "scale": "1 1 1", "angles": "0 0 0",
  "size": "1920 1080", "alpha": 1, "color": "1.00000 1.00000 1.00000",
  "visible": { "value": true, "user": "showbg" },
  "effects": [{
    "file": "effects/waterripple/effect.json", "id": 3, "visible": true,
    "passes": [{                                    // → 1re passe *à matériau* de l'effet
      "combos": { "MODE": 1 },
      "constantshadervalues": { "speed": { "value": 1.0, "user": "speed" },
                                "ripplestrength": 0.3 },
      "textures": [ null, "masks/masque_ripple" ]   // index 1 ; index 0 laissé tel quel
    }]
  }],
  "instance": { "textures": [ "monimage" ] }        // ne remplace PAS un index déjà défini (§12.1)
}
```

### 7.5 Calques d'animation (marionnette)

Élément de `animationlayers` :

| Clé | Mode | Défaut |
|---|---|---|
| `id` | R entier | — |
| `rate` | L | 1.0 |
| `visible` | L | **faux** |
| `blend` | L | 1.0 |
| `animation` | L | Entier 0 |

Le format des fichiers de marionnette (`puppet`, `.mdl`) relève d'une autre spec.

### 7.6 Son

| Clé | Mode | Défaut | Notes |
|---|---|---|---|
| `sound` | R tableau | — | Chaque élément : chaîne stricte (chemin d'asset audio). |
| `playbackmode` | O chaîne | absent | `"loop"` → lecture en boucle ; toute autre valeur ou absence → pas de boucle. |

Clés WE ignorées : `volume`, `startsilent`, `mintime`, `maxtime`, etc.

### 7.7 Texte

| Clé | Mode | Défaut | Notes |
|---|---|---|---|
| `text` | L! | — | Contenu ; vue texte. Peut porter un script (`script`, `scriptproperties`) : la valeur initiale sert de valeur de départ. |
| `font` | O chaîne | `""` | Chemin d'asset de police ; vide ou préfixe `systemfont_` → police système de repli. |
| `pointsize` | L | 32.0 | Taille en points (vue flottante). |
| `size` | O vec2 strict | (0,0) | Boîte englobante. Forme objet `{"value":…}` refusée (E2, Q19). |
| `scale` | L | vec3(1) | |
| `color` | Lc | blanc | |
| `alpha` | L | 1.0 | |
| `visible` | L | vrai | |
| `horizontalalign` | O chaîne | voir note | Sinon `alignment`, sinon `"center"`. Valeurs : `left`, `center`, `right`. |
| `verticalalign` | O chaîne | `"center"` | `top`, `center`, `bottom`. |
| `padding` | O vec2 strict | (0,0) | Scalaire accepté (même valeur sur x et y) ou `"x y"`. |

Les limites de texte (`limits…`) ne sont pas lues.

### 7.8 Particule (objet de scène)

| Clé | Mode | Défaut | Notes |
|---|---|---|---|
| `particle` | — | — | Chaîne = chemin du fichier de particules ; objet = définition en ligne (§7.9). |
| `scale` | L | vec3(1) | |
| `angles` | L | vec3(0) | |
| `visible` | L | vrai | |
| `parallaxDepth` | L | vec2(0) | |
| `instanceoverride` | facultatif | voir tableau | Multiplicateurs d'instance. |

Source de la définition : si `particle` est une chaîne non vide, le fichier est lu ; fichier
absent/illisible → W et définition vide `{}` ; JSON invalide → E1. Si `particle` est un objet, il
est utilisé directement.

`instanceoverride` :

| Clé | Si `instanceoverride` présent | Si absent |
|---|---|---|
| `enabled` | L, défaut **vrai** | Booléen faux |
| `alpha`, `size`, `lifetime`, `rate`, `speed`, `count` | L, défaut 1.0 | Flottant 1.0 |
| `color` (remplace la couleur), `colorn` (multiplie la couleur) | L, défaut vec3(1) | Flottant 1.0 (vue vec3 = (1,1,1)) |

Si `instanceoverride` n'est pas un objet, toutes ses clés prennent les défauts de la colonne
« présent ».

### 7.9 Fichier de particules

Clés de premier niveau :

| Clé | Mode | Défaut | Notes |
|---|---|---|---|
| `emitter` | T tableau | `[]` | Émetteurs (au singulier dans le format). |
| `initializer` | T tableau | `[]` | Initialiseurs. |
| `operator` | T tableau | `[]` | Opérateurs. |
| `renderer` | T tableau | voir note | Si vide/absent : un rendu `sprite` par défaut est ajouté. |
| `controlpoint` | T tableau | `[]` | Points de contrôle. |
| `children` | T tableau | `[]` | Systèmes enfants. |
| `material` | T chaîne | aucun | Chemin d'un **matériau** (§9), enveloppé dans un modèle dont tous les booléens sont faux et sans largeur/hauteur/marionnette. Fichier absent → W, pas de matériau ; JSON invalide → E1. Sans matériau la particule n'est **pas** instanciée au rendu. |
| `animationmode` | T chaîne | `"sequence"` | |
| `sequencemultiplier` | T nombre | 1.0 | |
| `maxcount` | T nombre (u32) | 100 | |
| `starttime` | T nombre (u32) | 0 | |
| `flags` | T nombre (u32) | 0 | |

Toute erreur de type JSON levée pendant l'analyse d'une particule (hors cas T) est journalisée
puis **E1**.

**Émetteur** (élément de `emitter`) :

| Clé | Lecture | Défaut |
|---|---|---|
| `id` | O entier | −1 |
| `name` | T chaîne | `""` |
| `directions` | V3 | (1, 1, 0) |
| `distancemin` | V3 | (0, 0, 0) |
| `distancemax` | V3 | (256, 256, 0) |
| `origin` | V3 | (0, 0, 0) |
| `sign` | tableau ≥ 3 entiers seulement | (0, 0, 0) |
| `instantaneous` | O u32 | 0 |
| `speedmin` / `speedmax` | O flottant | 0 / 0 |
| `rate` | O flottant | 10 |
| `controlpoint` | O entier | 0 |
| `flags` | O u32 | 0 |
| `cone` | O flottant | 0 |
| `delay` / `duration` | O flottant | 0 / 0 |
| `audioprocessingbounds` | V2 | (0.8, 1.0) |
| `audioprocessingexponent` | O entier | 2 |
| `audioprocessingfrequencystart` | O entier | 0 |
| `audioprocessingfrequencyend` | O entier | 1 |
| `audioprocessingmode` | O entier | 0 |
| `minperiodicdelay` / `maxperiodicdelay` | O flottant | 1.0 / 2.0 |
| `minperiodicduration` / `maxperiodicduration` | O flottant | 2.0 / 3.0 |

Lecture **V3** : absent → défaut ; chaîne → vec3 strict (nombre de composantes ≠ 3 → E2) ;
nombre → les trois composantes ; tableau d'au moins 3 nombres → 3 premiers ; autre → défaut.
Lecture **V2** : absent → défaut ; chaîne → vec2 strict ; tableau d'au moins 2 nombres → 2
premiers ; **nombre ou autre → défaut**.

**Initialiseurs** (clé `name`, O chaîne ; tous les champs sont des Réglages L, liables) :

| `name` | Champs (défauts) |
|---|---|
| `colorrandom` | `min` Lc (noir), `max` Lc (blanc) — souvent écrits en 0‑255 entiers (§0.7) |
| `sizerandom` | `min` 0, `max` 20, `exponent` 1 |
| `alpharandom` | `min` 0.05, `max` 1 |
| `lifetimerandom` | `min` 0, `max` 1 |
| `velocityrandom` | `min` vec3(−32), `max` vec3(32) |
| `rotationrandom` | `min` vec3(0), `max` (0, 0, 2π) |
| `angularvelocityrandom` | `min` (0, 0, −5), `max` (0, 0, 5), `exponent` 1 |
| `turbulentvelocityrandom` | `speedmin` 100, `speedmax` 250, `scale` 1, `offset` 0, `forward` (0, 1, 0), `timescale` 1, `phasemin` 0, `phasemax` 0.1, `right` (0, 0, 1) |
| `mapsequencearoundcontrolpoint` | `controlpoint` Entier 0, `count` Entier 1, `speedmin` vec3(0), `speedmax` vec3(100) |
| autre / absent | initialiseur ignoré silencieusement |

**Opérateurs** :

| `name` | Champs (défauts) |
|---|---|
| `movement` | `drag` 0, `gravity` vec3(0) |
| `angularmovement` | `drag` 0, `force` vec3(0) |
| `alphafade` | `fadeintime` 0.5, `fadeouttime` 0.5 |
| `sizechange` | `starttime` 0, `endtime` 1, `startvalue` 1, `endvalue` 0 |
| `alphachange` | `starttime` 0, `endtime` 1, `startvalue` 1, `endvalue` 0 |
| `colorchange` | `starttime` 0, `endtime` 1, `startvalue` vec3(1), `endvalue` vec3(1) |
| `turbulence` | `scale` 0.005, `speedmin` 500, `speedmax` 1000, `timescale` 0.01, `mask` (1, 1, 0), `phasemin` 0, `phasemax` 0, `audioprocessingmode` Entier 0, `audioprocessingbounds` vec2(0, 1), `audioprocessingexponent` 1.0, `audioprocessingfrequencystart` Entier 0, `audioprocessingfrequencyend` Entier 15 |
| `vortex`, `vortex_v2` | `controlpoint` **O entier** 0, `flags` **O entier** 0 (bit 1 = axe infini, bit 2 = conserver la distance au centre, bit 4 = forme d'anneau), `axis` (0, 0, 1), `offset` vec3(0), `distanceinner` 500, `distanceouter` 650, `speedinner` 2500, `speedouter` 0, `centerforce` 1, `ringradius` 300, `ringwidth` 50, `ringpulldistance` 50, `ringpullforce` 10, `audioprocessingmode` Entier 0, `audioprocessingbounds` vec2(0, 1) |
| `controlpointattract` | `controlpoint` **O entier** 0, `origin` vec3(0), `scale` 100, `threshold` 1000 |
| `oscillatealpha` | `frequencymin` 0, `frequencymax` 10, `scalemin` 0, `scalemax` 1, `phasemin` 0, `phasemax` 2π |
| `oscillatesize` | `frequencymin` 0, `frequencymax` 10, `scalemin` 0.8, `scalemax` 1.2, `phasemin` 0, `phasemax` 2π |
| `oscillateposition` | `frequencymin` 0, `frequencymax` 5, `scalemin` 0, `scalemax` 10, `phasemin` 0, `phasemax` 2π, `mask` (1, 1, 0) |
| autre / absent | opérateur ignoré silencieusement |

(2π = 6.2831855 en simple précision.)

**Rendus** (élément de `renderer`) :

| Clé | Lecture | Défaut |
|---|---|---|
| `name` | T chaîne | `"sprite"` (valeurs connues : `sprite`, `rope`, `ropetrail`) |
| `length` | O flottant | 1.0 si `ropetrail`, sinon 0.05 |
| `maxlength` | O flottant | 10 |
| `minlength` | O flottant | 0 |
| `subdivision` | O flottant | 4 si `rope`, sinon 1 |
| `segments` | O flottant | 4 |
| `uvscale` | O flottant | 1 |
| `uvscrolling` | O booléen | faux |
| `uvsmoothing` | O booléen | vrai |
| `fadealpha` | O booléen | faux |
| `fadesize` | O booléen | faux |

Rendu par défaut (si aucun) : `sprite` avec les défauts ci‑dessus (length 0.05, subdivision 1).

**Points de contrôle** (élément de `controlpoint`) :

| Clé | Lecture | Défaut |
|---|---|---|
| `id` | O entier | −1 |
| `flags` | O u32 | 0 |
| `offset` | chaîne : lue en flux tolérant aux blancs (3 flottants, manquants = 0) ; nombre : les trois composantes ; tableau : E2 (Q25) | (0, 0, 0) |
| `locktopointer` | O booléen | faux |

**Enfants** (élément de `children`) :

| Clé | Lecture | Défaut |
|---|---|---|
| `type` | T chaîne | `"static"` |
| `name` | T chaîne | `""` |
| `particle` | T chaîne | `""` (chemin ; **non chargé** à l'analyse) |
| `maxcount` | O entier | 20 |
| `controlpointstartindex` | O entier | 0 |
| `probability` | O flottant | 1.0 |
| `angles` | V3 | (0, 0, 0) |
| `origin` | V3 | (0, 0, 0) |
| `scale` | V3 | (1, 1, 1) |

Exemple minimal de fichier de particules :

```jsonc
{
  "material": "materials/particle/neige.json",
  "maxcount": 500,
  "emitter": [ { "name": "boxrandom", "rate": 40,
                 "distancemax": "960 540 0", "origin": "0 0 0" } ],
  "initializer": [ { "name": "sizerandom", "min": 4, "max": 12 },
                   { "name": "colorrandom", "min": "200 200 255", "max": "255 255 255" },
                   { "name": "lifetimerandom", "min": 3, "max": 6 } ],
  "operator": [ { "name": "movement", "gravity": "0 -40 0" },
                { "name": "alphafade", "fadeintime": 0.2, "fadeouttime": 0.8 } ]
  // "renderer" absent → un rendu "sprite" par défaut
}
```

### 7.10 Objets génériques

Un objet générique ne possède que les champs communs (§7.2). Il est créé au rendu comme nœud vide
(aucun dessin ; un avertissement « objet de remplacement » est journalisé), sert de parent de
transformation et d'objet scriptable (`origin`, `scale`, `angles`, `visible` de groupe). Il
figure dans l'ordre de rendu (§13.2) mais ne dessine rien.

---

## 8. `models/*.json`

| Clé | Mode | Défaut | Notes |
|---|---|---|---|
| `material` | R chaîne | — | Chemin du matériau (§9), chargé immédiatement ; échec → E1. |
| `solidlayer` | O booléen | faux | Calque plein : sans texture et sans taille, la taille devient celle de la scène. |
| `fullscreen` | O booléen | faux | Taille = taille de scène, origine = centre de la scène. |
| `passthrough` | O booléen | faux | Image « de passage » : sans effet, ou si tous ses effets sont invisibles à la mise en place, l'image n'est pas rendue. |
| `autosize` | O booléen | faux | Lu, sans effet. |
| `nopadding` | O booléen | faux | Lu, sans effet. |
| `width`, `height` | O entier | absents | Lus ; en pratique jamais utilisés (Q26). |
| `puppet` | O chaîne | absent | Chemin du fichier de marionnette (autre spec). |

Le chemin du modèle est mémorisé (affiché dans les dumps).

```jsonc
{ "material": "materials/fond.json", "fullscreen": false, "passthrough": false }
```

---

## 9. `materials/*.json`

| Clé | Mode | Notes |
|---|---|---|
| `passes` | R | Tableau de passes. Non‑tableau → **zéro passe** (une image dont le matériau n'a aucune passe est invalide : traiter comme E1, Q27). |

Passe de matériau :

| Clé | Mode | Défaut | Valeurs |
|---|---|---|---|
| `shader` | R chaîne | — | Nom du shader (sans extension). |
| `blending` | O chaîne | `"normal"` | `normal`, `translucent`, `additive` ; autre → W + `normal`. |
| `cullmode` | O chaîne | `"nocull"` | `nocull` (pas d'élimination), `normal` ; autre → W + `nocull`. |
| `depthtest` | O chaîne | `"disabled"` | `disabled`, `enabled` ; autre → W + `disabled`. |
| `depthwrite` | O chaîne | `"disabled"` | `disabled`, `enabled` ; autre → W + `disabled`. |
| `textures` | facultatif | `{}` | Carte de textures (§11.1). |
| `usertextures` | facultatif | `{}` | Carte de textures. |
| `combos` | facultatif | `{}` | Objet nom → entier strict (§11.2). |
| `constantshadervalues` | facultatif | `{}` | Objet nom → Réglage (§11.3). |

Valeurs numériques internes visibles **uniquement** dans le dump de structure (§15.2) :
blending normal=1, translucent=2, additive=3 ; cullmode normal=1, nocull=2 ; depthtest/depthwrite
disabled=1, enabled=2.

```jsonc
{
  "passes": [{
    "shader": "genericimage2",
    "blending": "translucent", "cullmode": "nocull",
    "depthtest": "disabled", "depthwrite": "disabled",
    "textures": [ "fond", null, "masks/m1" ],          // index 0, (1 vide), 2
    "combos": { "TRANSPARENCY": 1 },
    "constantshadervalues": { "alpha": 1.0, "color": "1 1 1" }
  }]
}
```

---

## 10. `effects/*.json`

| Clé | Mode | Défaut | Notes |
|---|---|---|---|
| `name` | O chaîne | `""` | |
| `description` | O chaîne | `""` | |
| `group` | O chaîne | `""` | |
| `preview` | O chaîne | `""` | |
| `dependencies` | facultatif | `[]` | Tableau de chaînes strictes (chemins) ; non‑tableau → vide. Mémorisé, sans autre effet. |
| `passes` | R | — | Tableau ; non‑tableau → aucune passe. |
| `fbos` | facultatif | `[]` | Tableau de FBO (ci‑dessous) ; non‑tableau → aucun. |

Passe d'effet :

| Clé | Mode | Notes |
|---|---|---|
| `material` | facultatif chaîne | Chemin de matériau (§9) chargé immédiatement (échec → E1). |
| `bind` | facultatif | Tableau de `{ "index": R entier, "name": R chaîne }`. Non‑tableau → vide. Index dupliqué : le premier gagne. `name` = nom de FBO/cible de rendu, ou `"previous"` (§12.3). |
| `command` | facultatif | Présent : `"copy"` → copie ; **toute autre valeur** → échange (« swap »). |
| `source` | R chaîne si `command`, sinon O chaîne | |
| `target` | R chaîne si `command`, sinon O chaîne | Cible de rendu de la passe. |

FBO :

| Clé | Mode | Défaut |
|---|---|---|
| `name` | R chaîne | — |
| `format` | O chaîne | `"rgba8888"` |
| `scale` | O flottant | 1.0 (diviseur de taille, spec rendu) |
| `unique` | O booléen | faux |

```jsonc
{
  "name": "Flou", "group": "blur", "dependencies": [ "shaders/effects/blur.frag" ],
  "fbos": [ { "name": "_rt_HalfCompoBuffer1", "scale": 2, "format": "rgba8888" } ],
  "passes": [
    { "material": "materials/effects/blur_h.json", "target": "_rt_HalfCompoBuffer1" },
    { "material": "materials/effects/blur_v.json",
      "bind": [ { "name": "_rt_HalfCompoBuffer1", "index": 0 } ] },
    { "command": "copy", "source": "previous", "target": "_rt_copie" }   // ne consomme pas de surcharge
  ]
}
```

---

## 11. Cartes de textures, combos, constantes

### 11.1 Carte de textures (`textures`, `usertextures`)

- Doit être un tableau ; sinon carte vide.
- L'**index d'emplacement** d'un élément est sa position dans le tableau (0, 1, 2…), y compris
  quand l'élément est ignoré.
- Élément `null` → ignoré. Chaîne vide → ignorée. Chaîne non vide → nom de texture.
- Objet avec `name` chaîne → ce nom ; objet sans `name` chaîne → ignoré. Tout autre type
  (nombre, booléen, tableau) → ignoré silencieusement.
- Résultat : association index → nom, triée par index.

### 11.2 Combos

Objet `nom → entier` (conversion stricte : nombre ou booléen ; chaîne → E1). Les noms sont
utilisés en **majuscules** lors de la compilation (§12.4).

### 11.3 Constantes (`constantshadervalues`)

Objet `nom → Réglage` analysé par §5.2 avec couleur attendue = **faux** : `"1 0 0"` → Vec3,
`0.5` → Flottant, `1` → Entier, `true` → Booléen, `{"value":…, "user":…}` → lié. Le nom est
l'**alias** de paramètre déclaré dans les annotations du shader (clé `material` des annotations,
spec shaders) — et non le nom de l'uniform GLSL.

---

## 12. Héritage et précédence des valeurs

### 12.1 Textures d'instance (objet image → première passe du modèle)

Les cartes `instance.textures` et `instance.usertextures` sont ajoutées respectivement à
`textures` et `usertextures` de la **première passe** du matériau du modèle de l'image, **sans
remplacer** un index déjà présent (seuls les index manquants sont ajoutés, Q14).

Texture principale d'une image : l'entrée de **plus petit index** de la carte `textures` de la
première passe (après fusion), résolue comme cible de rendu si elle commence par `_rt_` ou
`_alias_`, sinon comme fichier texture. Sans entrée : texture de remplacement vide (spec rendu).

### 12.2 Appariement des surcharges de passe aux passes d'effet

Pour un effet visible d'une image, avec la liste de surcharges `S[0..m-1]` (dans l'ordre de
`passes` de l'objet) et la liste de passes de l'effet `P[0..n-1]` (fichier d'effet) :

- un curseur `c` démarre à 0 ;
- pour chaque `P[i]`, dans l'ordre :
  - **sans `material`** : passe de commande. Exige `command`, `source`, `target`, et seule la
    commande « copie » est prise en charge ; sinon W et la passe est sautée. Une passe de copie
    produit une passe de rendu utilisant un shader de copie trivial (échantillonne l'emplacement 0
    et l'écrit tel quel), emplacement 0 = `source`, cible = `target`, mélange normal, sans
    élimination, sans profondeur. **Ne consomme pas** de surcharge.
  - **avec `material`** : pour **chaque** passe du matériau, une passe de rendu est créée avec la
    surcharge `S[c]` si `c < m` (sinon aucune), les `bind` de `P[i]` et la `target` de `P[i]`.
    Ensuite, si `c < m`, `c` avance de 1.
- Les surcharges excédentaires sont ignorées. Le champ `id` des surcharges n'intervient pas.

Ordre global des passes d'une image : passes du matériau du modèle (sans surcharge), puis pour
chaque effet visible dans l'ordre de `effects` ses passes (ci‑dessus), puis les passes de
compatibilité (§12.6).

### 12.3 Textures par emplacement (de la plus faible à la plus forte priorité)

Pour une passe de rendu, l'emplacement `k` reçoit la dernière définition trouvée dans :

1. textures par défaut déclarées dans les annotations du shader de sommets, puis du shader de
   fragments ;
2. `textures` de la passe de matériau ;
3. `usertextures` de la passe de matériau ;
4. `textures` de la surcharge ;
5. `usertextures` de la surcharge ;
6. `bind` de la passe d'effet : nom de FBO/cible de rendu, ou `"previous"` = la sortie de la passe
   précédente de la chaîne (à défaut l'entrée attendue).

Un nom commençant par `_rt_` ou `_alias_` est résolu comme cible de rendu ; sinon comme fichier
texture (§1.2). Une texture introuvable → W, l'emplacement garde la définition précédente.

(Pour la compilation du shader, l'ensemble « textures de passe » transmis est `textures` fusionné
avec `usertextures` de la passe, cette dernière prioritaire ; plus `textures` de la surcharge.)

### 12.4 Combos

Pour chaque nom (comparé en majuscules ASCII), la première source qui le définit gagne :

1. combos de la **surcharge** ;
2. combos de la **passe de matériau**, où `TEX0FORMAT` est forcé à 8 si la texture principale de
   l'objet est au format RG88 et à 9 si elle est au format R8 (cette valeur forcée remplace celle
   du matériau, mais pas celle d'une surcharge) ;
3. valeurs par défaut découvertes dans le shader (spec shaders).

### 12.5 Constantes

Pour chaque paramètre du shader (identifié par son alias, §11.3) :

1. valeur par défaut des annotations du shader ;
2. remplacée par `constantshadervalues` de la passe de matériau si le nom correspond ;
3. remplacée par `constantshadervalues` de la surcharge si le nom correspond.

Les noms sans paramètre correspondant sont ignorés. Le lien est **vivant** : l'uniform lit à
chaque image la vue du Réglage correspondant à son type GLSL (float → flottant, int → entier,
vec2/vec3/vec4 → vue vecN). Une modification de propriété utilisateur se répercute donc sur les
uniforms. Autre type d'uniform → W, défaut du shader conservé.

### 12.6 Passes synthétiques ajoutées par l'implémentation

Ajoutées après les passes d'effets, dans cet ordre :

1. **Compatibilité « teinte magenta »** (heuristique de l'implémentation de référence) : parmi les
   effets visibles (dans l'ordre), la première surcharge ayant le combo `COMPOSITE` = 2 et une
   constante `compositecolor` dont la vue vec3 vérifie r > 0.55, g < 0.25, b > 0.45 déclenche
   l'ajout d'une passe utilisant la première passe de `materials/effects/tint.json`, avec une
   surcharge : combo `BLENDMODE` = 30, constantes `color` = cette teinte (Vec3) et `alpha` = 1.0
   (Flottant). Recommandation : reproduire pour la fidélité visuelle (Q28).
2. **Mélange de couleur** : si la vue entière de `colorBlendMode` de l'image (au moment de la mise
   en place) est > 0, une passe utilisant la première passe de
   `materials/util/effectpassthrough.json` est ajoutée, avec une surcharge dont le combo
   `BLENDMODE` vaut cette valeur.
- **Particules** : le rendu des particules utilise une surcharge interne posant `THICKFORMAT` = 1,
  `SPRITESHEET` = 1 si la texture est une planche de sprites, `TRAILRENDERER` = 1 pour
  `ropetrail`, et remplace le shader par `genericropeparticle` pour les rendus `rope`
  (spec particules). Ce remplacement de shader n'est jamais lu depuis le JSON.
- Si une image a plus d'une passe, le mode de mélange de la première passe est déplacé vers la
  dernière et la première passe passe en mélange normal (spec rendu).

---

## 13. Construction, ordre de rendu, hiérarchie, visibilité

### 13.1 Ordre de création

Les objets sont parcourus dans l'ordre du fichier ; pour chacun, la procédure **créer(o)** :

1. Si un objet de même `id` a déjà été créé → rien.
2. Pour chaque identifiant de `o.dependencies` (dans l'ordre), en ignorant `o.id` lui‑même : si un
   objet de la scène a cet `id` (le **premier** de la liste ayant cet id), **créer** cet objet ;
   sinon ignorer silencieusement.
3. Si `o.parent` est défini : chercher l'objet de cet `id` ; absent → E1 (la scène ne se
   construit pas) ; sinon **créer** le parent.
4. Créer `o` selon son type. Particules ignorées si les particules sont désactivées dans les
   réglages de l'application, ou si leur matériau est absent (W). Si la mise en place d'un objet
   échoue, W et l'objet n'est pas enregistré (il pourra être retenté s'il est à nouveau référencé).

### 13.2 Ordre de rendu

Liste `L` vide ; pour chaque objet dans l'ordre du fichier, **ajouter(o)** :

1. Si `o` n'a pas été créé → rien (ses dépendances ne sont pas parcourues non plus).
2. Pour chaque identifiant de `o.dependencies` (sauf `o.id`) : si l'objet existe dans la scène →
   **ajouter**‑le (récursivement) ; sinon W.
3. Ajouter `o` à la fin de `L` si aucun objet de même `id` n'y figure déjà.

Les parents **n'influencent pas** l'ordre de rendu. L'objet bloom synthétique (§14), s'il existe,
est ajouté en dernier.

### 13.3 Héritage de transformation (parent → enfant)

Transformation locale d'un objet : origine = vue vec3 de `origin` ; échelle et angle (composante
z) = ceux de l'image pour une image, échelle du texte et angle 0 pour un texte, échelle/angles « de
groupe » sinon.

On remonte la chaîne des parents (au plus 32 niveaux ; au‑delà W et arrêt ; parent non créé →
arrêt). En partant de la racine, chaque enfant est composé sur le résultat du parent `(Op, Sp, θp)` :

- `dx, dy` = rotation d'angle `θp` du vecteur `(Oe.x · Sp.x, Oe.y · Sp.y)` ;
- origine = `(Op.x + dx, Op.y + dy, Op.z + Oe.z · Sp.z)` ;
- échelle = `Se ⊙ Sp` (produit composante par composante) ; angle = `θe + θp`.

Calculée à la construction de l'objet (spec rendu pour la mise à jour dynamique).

### 13.4 Visibilité

- Image/texte/particule : vue booléenne de leur propre `visible`.
- La visibilité **n'est pas héritée** du parent (Q16).
- La visibilité d'un effet d'image est évaluée **une fois** à la mise en place : un effet invisible
  à ce moment n'a aucune passe créée et ne réapparaît jamais (Q16).
- Image `passthrough` : non mise en place si elle n'a aucun effet ou si tous ses effets sont
  invisibles à la mise en place.

---

## 14. Objet bloom synthétique

Si la vue booléenne de `general.bloom` est vraie à la construction de la scène, l'implémentation
ajoute une image construite à partir d'un JSON synthétique équivalent à :

```jsonc
{
  "id": -1,                                   // conflit possible avec un id -1 réel (Q17)
  "name": "<nom interne libre>",
  "image": "<modèle virtuel>",                // → matériau virtuel : 1 passe, shader "genericimage2",
                                              //   textures ["_rt_FullFrameBuffer"], normal/nocull/disabled/disabled
  "visible": true,
  "origin": "<W/2> <H/2> 0",                  // W×H = taille de la scène (caméra) ; 6 décimales
  "scale": "1.0 1.0 1.0", "angles": "0.0 0.0 0.0",
  "size": "<W> <H>",
  "effects": [{
    "id": 15242000, "name": "",               // id arbitraire, sans incidence
    "file": "<effet virtuel>",
    "passes": [                               // 3 surcharges identiques, valeurs figées à la construction
      { "constantshadervalues": { "bloomstrength": S, "bloomthreshold": T } },
      { "constantshadervalues": { "bloomstrength": S, "bloomthreshold": T } },
      { "constantshadervalues": { "bloomstrength": S, "bloomthreshold": T } }
    ]
  }]
}
```

`S`, `T` = vues flottantes de `general.bloomstrength` / `general.bloomthreshold` au moment de la
construction (non dynamiques).

Effet virtuel (4 passes ; la 4ᵉ n'a pas de surcharge, Q21) :

| # | Matériau (asset WE) | Cible | Binds |
|---|---|---|---|
| 1 | `materials/util/downsample_quarter_bloom.json` | `_rt_4FrameBuffer` | 0 ← `_rt_FullFrameBuffer` |
| 2 | `materials/util/downsample_eighth_blur_v.json` | `_rt_8FrameBuffer` | 0 ← `_rt_4FrameBuffer` |
| 3 | `materials/util/blur_h_bloom.json` | `_rt_Bloom` | 0 ← `_rt_8FrameBuffer` |
| 4 | `materials/util/combine.json` | `_rt_FullFrameBuffer` | 0 ← cible composite « A » de l'objet bloom lui‑même (id −1), 1 ← `_rt_Bloom` |

Cibles de rendu créées après les objets de scène : `_rt_4FrameBuffer` (W/4 × H/4),
`_rt_8FrameBuffer` (W/8 × H/8), `_rt_Bloom` (W/8 × H/8), divisions entières, format ARGB8888,
UV bornées. Les chemins des fichiers virtuels (modèle, matériau, effet) sont à la discrétion de
l'implémentation mais ne doivent pas entrer en collision avec des assets réels.

---

## 15. Sorties de diagnostic

### 15.1 Liste des propriétés

Pour chaque propriété (ordre alphabétique), un bloc de lignes, `⇥` = tabulation :

| Genre | Lignes |
|---|---|
| curseur | `<nom> - slider` / `⇥Text: <libellé>` / `⇥Min: <min>` / `⇥Max: <max>` / `⇥Step: <pas>` / `⇥Value: <texte §4.3>` |
| booléen | `<nom> - boolean` / `⇥Text: …` / `⇥Value: <1|0>` |
| couleur | `<nom> - color` / `⇥Text: …` / `⇥Value: <r, g, b, a>` |
| combo | `<nom> - combo` / `⇥Text: …` / `⇥Value: …` / `Values: ` (sans tabulation) / puis par option triée `⇥⇥<valeur> = <libellé>` |
| texte | `<nom> - text` / `⇥Text: <libellé>` / `⇥Value: <libellé>` |
| scenetexture | `<nom> - scene texture` / `⇥Text: …` / `⇥Value: ` (toujours vide, Q10) |
| fichier/dossier | `<nom> - file` / `⇥Text: …` / `⇥Value: ` (vide) |
| textinput/raccourci | `<nom> - textinput` / `⇥Text: …` / `⇥Value: ` (vide) |

Min/Max/Step sont imprimés au format flottant court par défaut des flux (ex. `0`, `10`, `0.1`).

### 15.2 Dump de structure (option de débogage)

Arborescence textuelle, une tabulation par niveau d'indentation, booléens en `0`/`1`, modes de
matériau en valeurs numériques (§9) :

- vidéo : `Video wallpaper: <fichier>` ; web : `Web wallpaper: <fichier>` ;
- scène : `Scene wallpaper: `, `Objects count: N`, puis par objet `Object <id> <nom>:`,
  `Dependencies (<n>): ` suivi de `<id>, ` pour chacune ;
  - image : bloc modèle (`Model <fichier>:`, `Autosize:`, `Passthrough:`, `Fullscreen:`,
    `No padding:`, `Solid layer:`, `Width:`/`Height:` si présents, bloc matériau), puis
    `Image effects count: N` et par effet `Image effect <id>:`, `Default visibility status: <0|1>`,
    bloc effet (`Effect <nom>:`, `Description:`/`Dependencies count:`/`Group:`/`Preview:` si non
    vides, `Passes count:`, `Pass <i>:` numérotées à partir de 1 avec `Command:`, `Target:`,
    `Source:`, `Binds count:`, `Bind <index>: <nom>`, bloc matériau ; `FBOs count:`,
    `FBO <nom>:`, `Format:`, `Scale:`), puis `Effect overrides count: N` et par surcharge
    `Pass override <id>:`, `Textures count:`/`Texture <i>: <nom>`, `Combos count:`/`Combo <nom>=<v>`,
    `Constants count:`/`Constant <nom>=<texte §4.3>` ;
  - son : `Sounds: N` puis `Filename <chemin>` ;
  - bloc matériau : `Material <fichier>:`, `Passes count: N`, par passe `Pass <shader>:`,
    `Depth test:`, `Depth write:`, `Blend mode:`, `Culling mode:`, textures et combos
    (`Combo <nom>: <v>`) si non vides.
- Les particules et textes n'affichent que l'en‑tête d'objet et les dépendances.

---

## Annexe A — Paquet `.pkg`

Lecture : entiers 32 bits **petit‑boutistes** ; « chaîne dimensionnée » = u32 longueur + octets
bruts (sans terminateur).

| Ordre | Champ |
|---|---|
| 1 | chaîne dimensionnée d'en‑tête, doit **commencer** par `PKGV` (ex. `PKGV0001`…) sinon E1 |
| 2 | u32 `N` = nombre d'entrées |
| 3 | `N` × { chaîne dimensionnée `chemin`, u32 `offset`, u32 `taille` } |
| 4 | zone de données : commence immédiatement après la table ; l'entrée i occupe `[début_données + offset_i, + taille_i)` |

Les fichiers `scene.pkg` puis `gifscene.pkg` du dossier du fond d'écran sont montés à la racine du
VFS s'ils existent (spec VFS pour la priorité).

## Annexe B — Texture `.tex`

Types : u32/i32 petit‑boutistes, f32 IEEE‑754 petit‑boutiste, « magique » = 9 octets (8 caractères
ASCII + NUL), chaîne NUL = octets jusqu'au premier 0 (consommé, non inclus).

### B.1 En‑tête

| Champ | Contrainte |
|---|---|
| magique | `TEXV0005` sinon E1 |
| magique | `TEXI0001` sinon E1 |
| u32 format | ∈ {0 ARGB8888, 1 RGB888, 2 RGB565, 4 DXT5, 6 DXT3, 7 DXT1, 8 RG88, 9 R8, 10 RG1616f, 11 R16f, 12 BC7, 13 RGBa1010102, 14 RGBA16161616f, 15 RGB161616f, 0xFFFFFFFF inconnu} sinon E1 |
| u32 drapeaux | accepté si **< 524335** (somme des drapeaux connus), sinon E1 (Q22). Bits : 1 sans interpolation, 2 UV bornées, 4 animé (GIF), 8 bord borné, 32 vidéo, 524288 priorité alpha (RG88/R8 : alpha dans G/R) |
| u32 largeur de stockage, u32 hauteur de stockage | puissance de 2 en général |
| u32 largeur réelle, u32 hauteur réelle | |
| u32 | ignoré |

### B.2 Conteneur

| Champ | Notes |
|---|---|
| magique | `TEXB0001`, `TEXB0002`, `TEXB0003` ou `TEXB0004` ; autre → E1 |
| u32 nombre d'images | |
| (B0003) u32 format d'image source | −1 (0xFFFFFFFF) ou 0..36 (codes FreeImage ; 35 = WebP, réutilisé pour MP4) ; autre → E1 |
| (B0004) u32 format d'image source, u32 « est MP4 » (=1) | si format inconnu et MP4 → format MP4 (35). **Si le format n'est pas MP4, le conteneur est ensuite traité comme B0003** pour les mipmaps (Q22). |

### B.3 Images et mipmaps

Pour chaque image : u32 nombre de mipmaps, puis pour chaque mipmap :

1. (B0004 vidéo uniquement) u32 ignoré, u32 ignoré, chaîne NUL (JSON d'éditeur, mémorisée), u32
   ignoré ;
2. u32 largeur, u32 hauteur ;
3. (B0002, B0003, B0004) u32 compression, i32 taille décompressée ;
4. i32 taille compressée (= taille des données dans le fichier) ;
5. si compression = 0 : taille décompressée := taille compressée ;
6. si compression = 1 : lire `taille compressée` octets et décompresser en bloc **LZ4** vers
   `taille décompressée` octets (erreur → E1) ; sinon lire `taille décompressée` octets bruts.

### B.4 Animation (si drapeau 4)

| Champ | Notes |
|---|---|
| magique | `TEXS0001`, `TEXS0002`, `TEXS0003` ; autre → E1 |
| u32 nombre d'images d'animation | |
| (S0003) u32 largeur GIF, u32 hauteur GIF | |
| trames | S0001 : u32 index d'image, f32 durée, u32 x, u32 y, u32 largeur, u32 ignoré, u32 ignoré, u32 hauteur. S0002/S0003 : u32 index d'image, f32 durée, f32 x, f32 y, f32 largeur (axe x), f32 terme croisé 1, f32 terme croisé 2, f32 hauteur (axe y). |

S0001/S0002 : largeur/hauteur GIF := largeur/hauteur de la première trame (aucune trame :
comportement indéfini dans l'implémentation de référence, traiter comme E1).

### B.5 Planche de sprites déduite

Si au moins une trame, largeur et hauteur réelles > 0, et largeur `fw`/hauteur `fh` de la
première trame > 0 : `colonnes = round(largeur_réelle / fw)`, `lignes = round(hauteur_réelle /
fh)` ; si les deux sont > 0 et `colonnes × lignes ≥ nb_trames` : planche = (colonnes, lignes,
nb_trames, durée = somme des durées des trames).

### B.6 Métadonnées `.tex-json`

Fichier `materials/<nom>.tex-json`, facultatif ; toute erreur (absent, JSON invalide, mauvais
type) est ignorée silencieusement. Si `spritesheetsequences` est un tableau non vide, seul son
premier élément est lu : `frames` (entier, défaut 0), `width` (flottant, 0), `height`
(flottant, 0), `duration` (flottant, 1.0). Si `frames > 0`, `width > 0`, `height > 0` et
dimensions réelles > 0 : colonnes = round(largeur_réelle / width), lignes =
round(hauteur_réelle / height), nb_trames = frames, durée = duration — ces valeurs **remplacent**
celles de B.5.

---

## Questions ouvertes

Chaque point décrit un comportement de référence ambigu, non géré ou probablement bogué. Une
décision produit est attendue (reproduire à l'identique, corriger, ou rendre configurable).

- **Q1 — Couleurs entières vs flottantes.** Une chaîne de couleur sans `.` est lue en entiers
  0‑255 : `"1 1 1"` donne une couleur quasi noire. Cohérent pour `colorrandom` (WE écrit
  `"255 255 255"`), mais douteux pour une couleur d'image ou une propriété `color` écrite sans
  décimales. Reproduire tel quel ?
- **Q2 — Couleurs hexadécimales.** `#RRGGBB` est décalé d'un octet (R=0, A=BB) ; les formes à 8
  chiffres (et 3/4 chiffres développées) échouent dès que le premier octet ≥ 0x80 (dépassement
  d'entier signé). Corriger vers la sémantique CSS évidente ?
- **Q3 — Couleurs avec virgules.** `"1, 0, 0"` devient `"1  0  0"` (espaces doubles) → 4
  composantes comptées → alpha 0 et canaux décalés. Normaliser les séparateurs ?
- **Q4 — Séparateurs vectoriels.** Espaces multiples/initiaux/finaux faussent le comptage (§0.6).
  Reproduire ou tokeniser proprement ?
- **Q5 — `nearz`/`farz`/`fov`.** Lus dans `camera` alors que WE les écrit généralement dans
  `general` : les valeurs du fichier sont alors ignorées (défauts 0 / 1000 / 50). Lire aussi
  `general` ?
- **Q6 — Scènes en perspective.** `orthogonalprojection` est obligatoire et `null` → E1 ; les
  scènes 3D/perspective ne se chargent pas.
- **Q7 — `workshopid` numérique.** Converti via un entier signé 32 bits : les identifiants
  > 2³¹−1 (fréquents) sont altérés. De plus le compteur d'identifiants synthétiques est consommé
  même quand `workshopid` existe.
- **Q8 — Combos et types.** Les valeurs de combo sont stockées en texte : un champ numérique lié
  à un combo sans condition vaut 0. Les conditions ne fonctionnent qu'avec des propriétés
  textuelles (ignorées pour bool/slider/color). Faut‑il convertir le texte numérique ?
- **Q9 — `textinput` initial.** La valeur initiale est la sérialisation JSON (`"abc"` avec
  guillemets) alors qu'une surcharge CLI stocke le texte brut. Incohérent.
- **Q10 — Affichage vide.** Les propriétés `scenetexture`, `file`/`directory`, `textinput`
  affichent toujours une valeur vide dans la liste des propriétés.
- **Q11 — Propriété `text`.** Sans valeur : un Réglage lié à elle devient Nul (zéros/faux).
- **Q12 — Erreurs de type dans les lectures optionnelles (E2).** Dans l'implémentation de
  référence, un type inattendu dans une lecture optionnelle typée termine le processus (ex.
  `"value": 1` pour une propriété `bool`, `parent` en chaîne, `size` objet dans un texte).
  Recommandation : E1 (ou W + défaut) — à trancher.
- **Q13 — Surcharge CLI de curseur non numérique.** Erreur non rattrapée au démarrage.
- **Q14 — Textures d'instance.** Ne remplacent pas les index déjà définis par le matériau, ce qui
  contredit probablement l'intention (personnaliser la texture d'un modèle partagé).
- **Q15 — Commandes d'effet.** Toute valeur de `command` autre que `copy` est classée « échange »
  puis non prise en charge au rendu (passe sautée).
- **Q16 — Visibilité.** Pas d'héritage de la visibilité du parent ; visibilité des effets figée
  à la mise en place (pas de bascule dynamique via propriété ou script).
- **Q17 — Identifiants.** Identifiants dupliqués : seul le premier objet est créé/rendu. Cycles de
  dépendances → récursion infinie. L'id −1 est partagé par le mode dégradé (§7.2) et l'objet bloom
  synthétique (collision possible). Parent inexistant → scène entière en échec.
- **Q18 — Instantanés.** `size` d'image (et `size`/`padding` de texte) sont figés à l'analyse,
  avant application des surcharges CLI ; les liaisons de ces champs ignorent donc `--set-property`.
- **Q19 — Champs vecteurs stricts.** `size`/`padding` de texte, vecteurs de caméra, chaînes
  vectorielles des émetteurs n'acceptent ni la forme objet `{"value":…}` ni un mauvais nombre de
  composantes.
- **Q20 — Objets non pris en charge.** Lumières (`light`), lumières volumiques (`shape`) et
  modèles 3D deviennent des objets génériques sans rendu.
- **Q21 — Bloom.** Défauts `bloomstrength`/`bloomthreshold` = 0 (différents des valeurs usuelles
  de WE) ; valeurs figées à la construction ; la 4ᵉ passe (combinaison) ne reçoit pas les
  constantes.
- **Q22 — TEX.** `TEXB0004` non vidéo est traité comme `TEXB0003` pour les mipmaps (à confirmer
  sur des fichiers réels) ; validation des drapeaux par comparaison « < somme » et non par
  masque (rejette la combinaison de tous les drapeaux, accepte des bits inconnus) ; animation
  sans trame indéfinie.
- **Q23 — Valeur nulle.** Passer une Valeur à Nul ne vide pas sa vue texte.
- **Q24 — Booléen d'un flottant.** Vue booléenne par troncature (0.5 → faux) alors que les
  vecteurs testent `x ≠ 0` (0.5 → vrai).
- **Q25 — Particules.** `offset` de point de contrôle sous forme de tableau termine le processus ;
  `sign` d'émetteur n'accepte que les tableaux ; `audioprocessingbounds` numérique est ignoré.
- **Q26 — `width`/`height` du modèle.** Lus mais jamais utilisés : une texture de remplacement est
  toujours créée avant le repli qui les utiliserait.
- **Q27 — Matériau sans passe.** `passes` non‑tableau donne zéro passe ; le rendu d'une image dans
  ce cas n'est pas défini dans l'implémentation de référence.
- **Q28 — Heuristique « teinte magenta ».** Passe de compatibilité ajoutée selon des seuils de
  couleur arbitraires (§12.6) ; spécifique à certains contenus. Conserver ?
- **Q29 — Combos non numériques.** Une valeur de combo en chaîne (`"MODE": "1"`) provoque E1.
- **Q30 — `user` mal typé.** `user` numérique/booléen, ou `condition` numérique, provoque E1.
- **Q31 — Échecs de chargement en cascade.** Tout modèle/matériau/effet manquant référencé par une
  image fait échouer **tout** le fond d'écran (E1), alors que fichier/matériau de particules
  manquant n'est qu'un avertissement. Harmoniser ?
