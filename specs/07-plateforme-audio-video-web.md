# Spec 07 — Plateforme, sorties, entrées, audio, vidéo, web, médias

Statut : rédigée en salle blanche à partir de l'observation du comportement du projet de référence (Linux uniquement).
Public : implémenteurs Rust de Lumen (crates `lumen`, `lumen-platform`, `lumen-audio`, `lumen-video`, `lumen-web`).
Cette spec est autosuffisante et ne présuppose aucune bibliothèque particulière. Quand un comportement dépend d'un
protocole système public (X11, Wayland, PulseAudio, D-Bus/MPRIS), le protocole est nommé ; le choix de la crate est libre.

Conventions de lecture :

- « **DOIT** » = requis pour la parité ; « **DEVRAIT** » = recommandé ; « *[défaut amont]* » = comportement observé dans la
  référence qui ressemble à un défaut ; la décision de le reproduire ou non est posée dans les « Questions ouvertes » (QO).
- « Sortie » = écran physique (connecteur, ex. `HDMI-1`, `DP-2`, `eDP-1`).
- « Fond » = fond d'écran Wallpaper Engine (dossier contenant `project.json`).
- « Rendu hors écran du fond » = image intermédiaire dans laquelle le fond est rendu à sa résolution propre, puis recopiée
  (avec mise à l'échelle) vers chaque sortie.
- Coordonnées « GL » : origine en bas à gauche, Y vers le haut. Coordonnées « écran » : origine en haut à gauche, Y vers le bas.

---

## 1. Cycle de vie du processus

### 1.1 Ordre de démarrage

1. Analyse de la ligne de commande (§2). Toute erreur est fatale (§1.3).
2. Affichage inconditionnel sur la sortie standard d'une ligne « en cours d'exécution avec : » suivie de tous les arguments
   bruts séparés par des espaces (affichée même si la journalisation est désactivée).
3. Résolution du dossier d'assets (§3.3), validation du chemin de capture d'écran (§18.1).
4. Ouverture de la source de métadonnées média (§16). L'absence de bus de session D-Bus est **fatale** dans la référence.
5. Chargement des fonds (§4.3) : lecture et analyse de `project.json` de chacun. Échec = fatal.
6. Application des surcharges de propriétés et éventuel listage (§3.6).
7. Si au moins un fond chargé est de type web : initialisation du moteur web (§15).
8. Initialisation des playlists (§17).
9. Si `--list-properties` : sortie immédiate, code 0 (aucune fenêtre ni aucun son).
10. Installation des gestionnaires d'arrêt (SIGINT, SIGTERM).
11. Création du pilote de sortie selon le mode et `XDG_SESSION_TYPE` (§4.2), puis du détecteur de plein écran (§9).
12. Initialisation audio : capture spectrale (§13), détecteur « autre application joue » (§12), périphérique de sortie (§11).
13. Création d'un moteur de rendu par sortie / groupe span (§4.3, §4.4).
14. Si `--dump-structure` : impression de la structure de chaque fond sur la sortie standard (format : voir spec des données).
15. Boucle principale (§8) jusqu'à demande d'arrêt.
16. Nettoyage, message « arrêt » sur la sortie standard, code de sortie 0.

### 1.2 Arrêt

- SIGINT / SIGTERM : message indiquant le numéro du signal, puis sortie propre de la boucle (pas d'arrêt brutal).
- Fermeture de la fenêtre (mode fenêtré) ou perte de la connexion Wayland : message « arrêt demandé par le pilote »,
  sortie propre.
- Il n'existe **aucune** interface de contrôle à chaud (pas d'IPC, pas de rechargement, pas de SIGHUP) : les réglages sont
  figés au démarrage.

### 1.3 Erreurs fatales et codes de sortie

- Code 0 : arrêt normal, `--help`, `--list-properties`.
- Code 1 : toute erreur fatale. Le message d'erreur est écrit sur la sortie d'erreur ; s'il survient pendant l'analyse
  des arguments, il est suffixé par une invitation à lancer `<argv0> --help`. *[défaut amont]* : la plupart des messages
  fatals apparaissent deux fois sur la sortie d'erreur (une fois au moment de l'erreur, une fois au niveau supérieur).

---

## 2. Ligne de commande

### 2.1 Grammaire générale

- Syntaxe : `lumen [options] [<id_ou_chemin>]`.
- Les arguments inconnus sont **ignorés silencieusement** (nécessaire car le moteur web de la référence relance le même
  exécutable avec des options internes, §15.7).
- `-h` / `--help` affiche l'aide (groupes : fond, performance, son, capture, contenu, configuration du fond, débogage)
  et des exemples d'usage, puis quitte avec le code 0. Pas d'option `--version`.
- Les options sont traitées **dans l'ordre d'apparition** ; plusieurs options s'appliquent « à la dernière sortie
  désignée » (§2.3).

### 2.2 Table des options

| Option | Argument | Défaut | Effet |
|---|---|---|---|
| *(positionnel)* | ID Workshop ou chemin | vide | Fond par défaut (utilisé par la fenêtre et par les sorties sans `--bg`). |
| `-w`, `--window` | `XxYxLxH` | — | Mode fenêtre explicite : position (X,Y) et taille (L,H). Une seule fois. |
| `-r`, `--screen-root` | nom de sortie | — | Mode fond de bureau ; désigne la sortie que ciblent les options suivantes. Répétable. |
| `--screen-span` | `A,B[,C…]` | — | Mode fond de bureau ; un seul fond étiré sur ≥ 2 sorties. Répétable. |
| `-b`, `--bg` | ID ou chemin | — | Fond de la dernière sortie / du dernier groupe désigné. Répétable. |
| `--playlist` | nom | — | Playlist lue dans la configuration de Wallpaper Engine (§17). Répétable. |
| `--scaling` | `stretch` \| `fit` \| `fill` \| `default` | `default` | Mode de mise à l'échelle (§4.5). Répétable. |
| `--clamp` | `clamp` \| `border` \| `repeat` | `clamp` | Mode de bord des UV (§4.5). Répétable. |
| `--layer` | `background` \| `bottom` \| `top` \| `overlay` | `bottom` | Wayland : couche layer-shell (§6). |
| `-f`, `--fps` | entier | 30 | Plafond d'images par seconde (§8). |
| `--hwdec` | `auto` \| `nvdec-copy` \| `no` | `auto` | Décodage vidéo matériel (§14). |
| `--no-fullscreen-pause` | — | pause active | Désactive la pause en plein écran (§9). |
| `--fullscreen-pause-only-active` | — | non | Wayland : ne compter que les fenêtres plein écran actives. |
| `--fullscreen-pause-ignore-appid` | sous-chaîne | — | Wayland : ignorer les fenêtres dont l'app_id contient la valeur. Répétable ; valeur vide ignorée. |
| `-v`, `--volume` | entier | 15 | Volume global, borné à [0, 128] après analyse. |
| `-s`, `--silent` | — | non | Coupe tout le son produit par les fonds (§11, §14). |
| `--noautomute` | — | automute actif | Ne coupe plus le son quand une autre application joue (§12). |
| `--no-audio-processing` | — | traitement actif | Désactive la capture spectrale (§13). |
| `--screenshot` | chemin | — | Active la capture d'écran vers ce fichier (§18). |
| `--screenshot-delay` | entier ≥ 0 | 5 | Nombre d'images avant la capture, **borné à 5 au maximum**. |
| `--assets-dir` | chemin | auto | Dossier `assets` de Wallpaper Engine (§3.3). |
| `--disable-particles` | — | non | Aucun système de particules n'est créé. |
| `--disable-mouse` | — | souris active | Désactive l'interaction souris (§10.5). |
| `--disable-parallax` | — | parallaxe active | Désactive la parallaxe de caméra (§10.3). |
| `-l`, `--list-properties` | — | non | Liste les propriétés utilisateur puis quitte (§3.6). |
| `--set-property`, `--property` | `clé[=valeur]` | — | Surcharge une propriété ; sans `=`, la valeur est `"1"`. Répétable. |
| `-z`, `--dump-structure` | — | non | Imprime la structure analysée des fonds (§20). |
| `--render-debug` | mode | — | Débogage de rendu (§20). Répétable. |

Groupes mutuellement exclusifs (erreur fatale si les deux sont présents) :

- `--window` et `--screen-root` ;
- `--volume` et `--silent`.

Autres incompatibilités fatales :

- `--window` après un `--screen-root` ou `--screen-span` (« impossible d'être à la fois fond et fenêtre »), et inversement ;
- `--window` donné deux fois ;
- `--window` vide ou sans trois séparateurs `x` (« format attendu XxYxLxH ») ; chaque champ est lu comme entier décimal
  signé avec la tolérance d'une conversion « préfixe numérique » (les caractères non numériques terminent le nombre) ;
- `--screen-root` répété pour la même sortie, ou visant une sortie déjà membre d'un groupe span ;
- `--screen-span` : moins de deux noms non vides (les éléments vides entre virgules sont ignorés), doublon dans le groupe,
  sortie déjà configurée individuellement ou déjà dans un autre groupe ;
- valeur hors liste pour `--scaling`, `--clamp`, `--layer`, `--hwdec` ;
- `--render-debug` invalide ou à identifiant non numérique / hors plage ;
- aucun fond résolu à la fin de l'analyse (« au moins un fond doit être spécifié »).

Validations post-analyse :

- volume ← min(max(volume, 0), 128) ;
- délai de capture ← min(délai, 5) ;
- un `--fps` nul ou négatif n'est pas validé par la référence (QO-1).

### 2.3 Ciblage « dernière sortie désignée »

L'analyseur maintient une variable « cible courante », initialement vide.

- `--screen-root N` : passe en mode fond de bureau ; enregistre N avec fond vide ; **copie** le mode d'échelle et le mode
  de bord *globaux courants* dans les réglages propres à N ; cible courante ← N.
- `--screen-span A,B` : passe en mode fond de bureau ; crée un groupe (sorties dans l'ordre donné, échelle et bord copiés
  des valeurs globales courantes, fond vide) ; cible courante ← clé synthétique `span:` + valeur brute de l'argument
  (ex. `span:A,B`), enregistrée comme une « sortie » à fond vide.
- `--bg V` : fond de la cible courante ← V résolu (§3.1) ; **et** fond par défaut ← V. Si la cible est un groupe span,
  le fond du dernier groupe créé ← V. Sans cible (aucun `-r`/span auparavant), V est rangé sous une clé vide, sans effet
  visible en mode bureau, mais il devient le fond par défaut (utilisable en mode fenêtre).
- `--scaling M` / `--clamp M` : si le mode courant est « fond de bureau », s'applique à la cible courante (et au dernier
  groupe span si la cible en est un) ; sinon (mode fenêtre, ou avant tout `-r`), modifie la valeur **globale**, qui sert
  de défaut aux `-r`/spans **suivants** et à la fenêtre.
- `--playlist P` : sans cible, devient la playlist de la fenêtre et, si aucun fond par défaut n'existe encore, son
  premier élément devient le fond par défaut ; avec cible, devient la playlist de la cible, son premier élément devient le
  fond de la cible, et le fond par défaut s'il était vide.
- Positionnel : fond par défaut ← valeur résolue (si non vide). Comme les actions sont exécutées dans l'ordre, le
  **dernier** de {positionnel, `--bg`} rencontré gagne pour le fond par défaut.

Exemples de référence (comportement attendu) :

- `lumen 2317494988` → aperçu en fenêtre.
- `lumen -r HDMI-1 --bg 2317494988 --scaling fill --clamp border` → HDMI-1, remplissage, bords transparents.
- `lumen -r HDMI-1 -r HDMI-2 2317494988` → même fond sur les deux sorties.
- `lumen -r HDMI-1 --bg A -r HDMI-2 --bg B` → A sur HDMI-1, B sur HDMI-2.
- `lumen --screen-span HDMI-1,HDMI-2 --bg 2317494988 --scaling fill` → un fond étiré sur deux sorties.
- `lumen --scaling stretch -r eDP-1 --bg X --scaling fill -r HDMI-1 --bg Y` → **eDP-1 finit en `fill`** (le second
  `--scaling` vise la cible courante eDP-1) et **HDMI-1 en `stretch`** (il copie la valeur globale, restée `stretch`,
  au moment du `-r HDMI-1`). C'est l'inverse de ce que suggère la documentation utilisateur de la référence, mais c'est
  le comportement observable (QO-2). Pour obtenir une échelle par sortie, l'utilisateur doit écrire `--scaling` **après**
  le `-r` concerné.

### 2.4 Réglages effectifs et défauts

| Réglage | Défaut |
|---|---|
| Mode | fenêtre normale (640×480) |
| Géométrie fenêtre explicite | (0,0,0,0) |
| Échelle globale / bord global | `default` / `clamp` |
| Couche Wayland | `bottom` |
| FPS | 30 |
| Décodage matériel | `auto` |
| Pause plein écran / seulement si actif / app_id ignorés | oui / non / aucun |
| Son activé / volume / automute / traitement audio | oui / 15 / oui / oui |
| Souris / parallaxe désactivée | oui / non |
| Capture / délai / chemin | non / 5 / vide |
| Particules désactivées / listage / dump | non / non / non |

---

## 3. Résolution des fonds

### 3.1 ID Workshop ou chemin

- Une valeur **sans aucun caractère `/`** est un ID Workshop (même si elle n'est pas numérique). Elle est résolue
  immédiatement (pendant l'analyse) en dossier : pour chaque racine Steam candidate (ordre ci-dessous),
  `$HOME/<racine>/431960/<id>` ; le premier qui existe **et** est un dossier est retenu. Aucun → erreur fatale
  « dossier workshop introuvable pour l'application 431960 et le contenu <id> ».
- Une valeur contenant `/` est un chemin, utilisé tel quel (relatif au répertoire courant s'il n'est pas absolu ;
  `./mon_fond` fonctionne, `mon_fond` est traité comme un ID).
- `$HOME` doit exister et être un dossier, sinon erreur fatale.

Racines Workshop (relatives à `$HOME`, dans cet ordre) :

1. `.local/share/Steam/steamapps/workshop/content`
2. `.steam/steam/steamapps/workshop/content`
3. `.var/app/com.valvesoftware.Steam/.local/share/Steam/steamapps/workshop/content`
4. `snap/steam/common/.local/share/Steam/steamapps/workshop/content`

Racines « applications » (relatives à `$HOME`, dans cet ordre — **ordre différent** du Workshop) :

1. `.steam/steam/steamapps/common`
2. `.local/share/Steam/steamapps/common`
3. `.var/app/com.valvesoftware.Steam/.local/share/Steam/steamapps/common`
4. `snap/steam/common/.local/share/Steam/steamapps/common`

Les bibliothèques Steam additionnelles (autres disques) ne sont **pas** consultées par la référence (QO-3).

### 3.2 Dossier d'installation de Wallpaper Engine

`<racine application>/wallpaper_engine/` : premier candidat existant et qui est un dossier. Sert pour `assets` (§3.3)
et pour `config.json` (§17).

### 3.3 Dossier d'assets

Ordre de résolution :

1. `--assets-dir` s'il est fourni (non vérifié à ce stade ; message informatif « assets utilisés depuis … »).
2. `<installation WE>/assets` s'il existe.
3. `<dossier de l'exécutable>/assets` (sans vérification).

Si, au chargement d'un fond, le dossier retenu ne peut pas être monté → erreur fatale « impossible de trouver un dossier
d'assets valide, résolu en <chemin> ».

### 3.4 Espace de fichiers d'un fond

Chaque fond dispose de son propre espace de fichiers virtuel ; une recherche parcourt les montages **dans l'ordre
ci-dessous et le premier qui contient le fichier gagne** :

1. fichiers virtuels en mémoire injectés par le moteur (effets internes, voir spec de rendu) ;
2. la pochette média courante, accessible sous le nom spécial `$mediaThumbnail` (§16.4) ;
3. le dossier du fond ;
4. `scene.pkg` du dossier du fond, s'il existe ;
5. `gifscene.pkg` du dossier du fond, s'il existe ;
6. le dossier d'assets (obligatoire) ;
7. le **répertoire de travail courant** du processus.

Un chemin relatif non trouvé est retenté en le préfixant de `/`.

### 3.5 Champs de `project.json` utilisés par cette spec

- `type` (obligatoire, insensible à la casse) : `scene`, `video` ou `web` ; autre → erreur fatale « type non supporté ».
- `file` (obligatoire) : fichier principal (scène, vidéo ou page HTML).
- `title` (obligatoire).
- `workshopid` (optionnel, nombre ou chaîne) : identifiant ; absent → identifiant synthétique négatif décroissant
  (`-1`, `-2`, …) par fond chargé. Sert au schéma d'URL web (§15.2).
- `general.supportsaudioprocessing` (booléen, défaut faux) : le fond demande l'analyse spectrale (§13).
- `general.properties` : propriétés utilisateur (format : spec des données/propriétés).

Le chargement vérifie aussi, lors d'un changement de playlist, que `project.json` contient `type` et `file` (§17.4).

### 3.6 Propriétés utilisateur (`--set-property`, `--list-properties`)

- Les surcharges sont indexées par clé exacte (sensible à la casse). Pour chaque fond chargé, et pour chaque propriété
  dont la clé correspond, la valeur textuelle est appliquée avec la provenance « utilisateur » (l'interprétation du
  texte selon le type est définie par la spec des propriétés) ; un message informatif « application de la surcharge
  pour <clé> » est émis. Les surcharges s'appliquent aussi aux fonds chargés plus tard par une playlist.
- `--list-properties` : pour chaque fond (ordre alphabétique des clés de sortie), pour chaque propriété (ordre
  alphabétique des clés), après application des surcharges, impression d'un bloc « `<clé> - <type>` » suivi de lignes
  indentées par tabulation (texte, bornes, pas, valeur, choix…), puis sortie code 0. Le format détaillé des blocs est
  celui de la spec des propriétés.
- Les surcharges ne sont **pas** transmises aux pages web (§15.5).

---

## 4. Modes de sortie, multi-écran et mise à l'échelle

### 4.1 Les trois modes

| Mode | Déclenché par | Sorties |
|---|---|---|
| Fenêtre normale | aucun `-w`/`-r`/span | une fenêtre redimensionnable, viewport unique nommé `default` |
| Fenêtre explicite | `-w` | une fenêtre sans décoration, non redimensionnable, toujours au premier plan, placée/taillée selon `-w` |
| Fond de bureau | `-r` ou `--screen-span` | une surface par sortie demandée (X11 ou Wayland) |

### 4.2 Choix du pilote

- `XDG_SESSION_TYPE` **doit** être défini, sinon erreur fatale (détection du serveur d'affichage impossible), quel que
  soit le mode.
- Modes fenêtre : toujours le pilote « fenêtre » générique (OpenGL 3.3 core, MSAA ×4 demandé), quelle que soit la session.
- Mode bureau : `x11` → sortie X11 (§5) ; `wayland` → sortie Wayland (§6) ; autre valeur → erreur fatale « aucun pilote
  pour ce mode et ce type de session ». Le support X11 et Wayland est optionnel à la compilation dans la référence.
- Détecteur plein écran : choisi d'après `XDG_SESSION_TYPE` **aussi en mode fenêtre** (§9).

### 4.3 Association sortie → fond

- Modes fenêtre : un seul fond, clé `default` : premier élément de la playlist de fenêtre si elle existe et n'est pas vide,
  sinon le fond par défaut.
- Mode bureau : pour chaque sortie enregistrée par `-r` (hors clés `span:`), son fond propre, ou le fond par défaut s'il
  est vide. Pour chaque groupe span, son fond propre ou le fond par défaut ; le fond est chargé **une seule fois** pour
  le groupe.
- Chaque sortie reçoit son propre moteur de rendu (même si deux sorties affichent le même fond, il est instancié deux
  fois) ; un groupe span partage **un** moteur de rendu entre toutes ses sorties.
- Pour chaque sortie : mode d'échelle = réglage propre à la sortie, sinon global ; idem pour le bord.
- Les sorties demandées mais absentes sont ignorées ; si **aucune** sortie demandée n'est trouvée → erreur fatale listant
  les sorties détectées et les sorties demandées (y compris celles des groupes span).
- Les sorties sont parcourues par ordre alphabétique de nom (impacte l'ordre de rendu et la composition des captures).

### 4.4 Groupes span

- Boîte englobante : union des rectangles (position globale, taille logique) de chaque sortie du groupe trouvée ; une
  sortie introuvable est journalisée et exclue ; aucune trouvée → groupe ignoré (erreur journalisée).
  - X11 : position globale = position du CRTC, taille logique = taille du CRTC.
  - Wayland : position globale = position logique xdg-output (à défaut, position de la géométrie wl_output) ; taille
    logique = taille logique xdg-output, à défaut taille fournie par la configuration layer-shell. Sans gestionnaire
    xdg-output, un avertissement indique que les positions seront fausses.
- Le fond est rendu une fois par image (une seule passe de scène même s'il alimente N sorties).
- UV de base : calculés par le mode d'échelle du groupe (§4.5) en prenant la boîte englobante comme viewport virtuel.
- Tranche d'une sortie, avec boîte (bx, by, bw, bh), position (gx, gy), taille logique (lw, lh), UV de base
  (u0,u1,v0,v1) :
  - rl = (gx − bx)/bw ; rr = (gx + lw − bx)/bw ; rt = (gy − by)/bh ; rb = (gy + lh − by)/bh
  - u_début = u0 + rl·(u1 − u0) ; u_fin = u0 + rr·(u1 − u0)
  - v_début = v0 + rt·(v1 − v0) ; v_fin = v0 + rb·(v1 − v0)
- Pour une page web en span, la taille de vue du navigateur est celle de la boîte englobante.

### 4.5 Mise à l'échelle (UV) et bords

Le fond est rendu hors écran à sa taille propre (P_l × P_h) : scène = taille de projection de la caméra ; vidéo = taille
d'affichage du flux décodé ; web = taille du viewport. Il est recopié sur le viewport (V_l × V_h, pixels physiques) par un
quad plein écran dont les coordonnées de texture sont (u_début..u_fin, v_début..v_fin), filtrage linéaire, sans mélange.

Inversion verticale « vflip » : vraie pour fenêtre et Wayland, fausse pour X11 (car l'image X11 est relue à l'envers,
§5). Réinitialisation : u ∈ [0,1] ; v ∈ [0,1] si vflip, sinon v_début = 1, v_fin = 0.

Deux primitives (les divisions viewport/projection sont flottantes, les tailles « nouvelles » sont tronquées en entier) :

- **Recadrage horizontal**(pw, ph) : nl = ent(V_h / ph · pw) ; c = nl/2 ; u_début = (c − V_l/2)/nl ;
  u_fin = (c + V_l/2)/nl.
- **Recadrage vertical**(pw, ph) : nh = ent(V_l / pw · ph) ; c = nh/2 ; bas = (c − V_h/2)/nh ; haut = (c + V_h/2)/nh ;
  si vflip : v_début = bas, v_fin = haut ; sinon v_début = haut, v_fin = bas.

Modes :

| Mode CLI | Algorithme |
|---|---|
| `stretch` | réinitialisation seule (déformation). |
| `fill` | m = max(V_l/P_l, V_h/P_h) ; pw = ent(P_l·m), ph = ent(P_h·m) ; si pw ≠ V_l → recadrage horizontal(pw,ph) ; sinon si ph ≠ V_h → recadrage vertical(pw,ph). Résultat : couverture avec rognage. |
| `fit` | idem avec m = min(…). Les UV sortent de [0,1] : bandes dont l'aspect dépend du mode de bord. |
| `default` | si (V_h > V_l et P_l ≥ P_h) ou (V_l > V_h et P_h > P_l) → recadrage horizontal(P_l,P_h) ; si (V_l > V_h et P_l ≥ P_h) ou (V_h > V_l et P_h > P_l) → recadrage vertical(P_l,P_h). Viewport carré → stretch. |

Lecture de `default` : viewport paysage + fond paysage → ajusté en largeur (bandes haut/bas si le fond est plus large,
rognage vertical sinon) ; viewport paysage + fond portrait → ajusté en hauteur (bandes latérales) ; viewport portrait +
fond paysage → ajusté en hauteur (rognage latéral) ; portrait/portrait → ajusté en largeur.

Les UV ne sont recalculés que si la taille du viewport, la taille du fond ou vflip change.

Modes de bord (appliqués à l'image hors écran du fond, donc visibles dans les bandes de `fit`/`default`) :

| CLI | Comportement hors [0,1] |
|---|---|
| `clamp` (défaut) | répétition du pixel de bord (étirement des bords). |
| `border` | noir transparent (affiché noir). |
| `repeat` | pavage. |

---

## 5. Sortie X11 (fond de bureau)

- Connexion au serveur X par défaut. Les erreurs X (y compris d'E/S) sont **interceptées et ignorées** (jamais fatales) ;
  si la plateforme le permet, une erreur d'E/S critique déclenche une tentative de réinitialisation de la sortie.
- XRandR requis ; absent ou ressources indisponibles → erreur journalisée « exécution en mode fenêtre » et la sortie
  reste sans viewport (rien ne s'affiche, l'application continue).
- Taille totale = taille de l'écran X (racine).
- Pour chaque sortie RandR **connectée** ayant un CRTC actif, et dont le nom est demandé (`-r` ou groupe span) :
  message « sortie demandée trouvée : NOM -> x×y:l×h » et création d'un viewport (x, y, l, h) du CRTC.
- Fenêtre GL cachée (jamais affichée), redimensionnée à la taille totale, classe/instance WM « linux-wallpaperengine »,
  titre « wallpaperengine » (QO-4 sur l'identité).
- Création d'un pixmap racine de profondeur 24, taille totale, rempli de noir, défini comme fond de la fenêtre racine.
- À chaque image :
  1. effacement complet du tampon GL ;
  2. rendu de chaque viewport à sa position dans le tampon GL (coordonnées X utilisées comme coordonnées GL) ;
  3. relecture de tout le tampon en BGRA 8 bits (la ligne 0 relue devient la ligne du haut de l'image X : d'où vflip
     faux) ;
  4. copie de l'image dans le pixmap ;
  5. propriétés `_XROOTPMAP_ID` et `ESETROOT_PMAP_ID` de la racine ← ce pixmap (permet aux compositeurs type picom
     d'afficher le fond et de se rafraîchir) ;
  6. effacement/repeinte de la fenêtre racine, vidage du tampon de requêtes.
- Conséquence : les sorties **non demandées** reçoivent du noir (toute la surface est réécrite).
- Limite connue : invisible si un environnement de bureau dessine son propre fond (GNOME, KDE, gestionnaire de
  fichiers).
- Pas de prise en charge du branchement à chaud.

## 6. Sortie Wayland (layer-shell)

### 6.1 Connexion et globals

- Connexion à l'affichage par défaut ; échec → fatal.
- Globals liés (version) : compositeur (4), mémoire partagée (1), chaque `wl_output` (4 — le nom de connecteur est requis),
  `zwlr_layer_shell_v1` (1), siège (1), `zxdg_output_manager_v1` (min(annoncée, 3)).
- Compositeur, mémoire partagée, layer-shell et au moins une sortie sont obligatoires, sinon fatal (« impossible de se
  lier aux interfaces requises »).
- Si le gestionnaire xdg-output existe, un objet xdg-output est créé par sortie (positions/tailles logiques).
- Contexte OpenGL **de bureau** 3.3 core via EGL plateforme Wayland, configuration fenêtre avec MSAA ×4 ; échecs → fatal.

### 6.2 Surfaces

Seules les sorties dont le nom est demandé (`-r` ou groupe span) reçoivent une surface ; aucune → fatal avec la liste
des sorties détectées / demandées. Pour chacune :

- surface layer-shell sur la sortie, couche selon `--layer` (défaut `bottom`), espace de noms « linux-wallpaperengine »
  (QO-4 ; certains compositeurs ont des règles qui s'appuient dessus, ex. niri + `background` + règle
  `place-within-backdrop`) ;
- ancrée aux quatre bords, taille demandée 0×0 (= toute la sortie), zone exclusive −1 (passe sous les panneaux),
  aucune interactivité clavier ;
- région opaque = toute la surface ;
- région d'entrée = toute la surface si la souris est activée, **vide** sinon (clics traversants) ;
- tampon de taille (taille × échelle entière de la sortie) ; échelle de tampon = échelle de la sortie ; pas de prise en
  charge de l'échelle fractionnaire ;
- intervalle d'échange 0 (la cadence vient des rappels de trame, §8.2) ;
- curseur : thème `XCURSOR_THEME` (défaut du système si absent), taille `XCURSOR_SIZE` (défaut 24) × échelle, image
  `left_ptr`, appliquée à l'entrée du pointeur ; échec de chargement du thème → fatal.

### 6.3 Événements

- Configuration layer-shell (l, h) : taille logique ← (l, h) ; viewport ← (0, 0, l·échelle, h·échelle) ; redimension du
  tampon ; acquittement.
- Fermeture de la surface par le compositeur : erreur journalisée, libération des ressources de cette sortie, retrait de
  la liste ; l'application continue (QO-5).
- Changement de mode / d'échelle / de nom / de taille logique d'une sortie : recalcul de la liste des viewports et de la
  taille totale. *[défaut amont]* un changement de mode écrase la taille logique par la taille en pixels du mode (QO-6).
- Taille totale (utilisée pour les captures) = somme des largeurs physiques, hauteur = max des hauteurs physiques.
- Sorties ajoutées/retirées après le démarrage : non gérées.
- Après chaque rendu : présentation, échelle de tampon, dommage sur tout le tampon, validation, et demande d'un nouveau
  rappel de trame.

## 7. Mode fenêtré

- Contexte OpenGL 3.3 core, MSAA ×4 demandé ; titre « wallpaperengine », classe/instance WM X11
  « linux-wallpaperengine » (QO-4).
- Fenêtre initiale 640×480, cachée à la création puis affichée.
- Fenêtre explicite : position et taille fixées depuis `-w` (le repositionnement est rejoué lors d'une réinitialisation).
- À chaque image, la taille du viewport `default` suit la taille réelle du tampon de la fenêtre (le contenu est
  ré-étiré si le gestionnaire de fenêtres la redimensionne).
- vflip vrai. Pas d'image relue (sauf capture).

---

## 8. Boucle principale, temps et cadence

### 8.1 Une itération (hors pause)

1. Heure du jour : `g_Daytime` = (heure·60 + minute)/1440 en heure locale.
2. Temps : `g_TimeLast` ← `g_Time` ; `g_Time` ← secondes écoulées depuis le démarrage du pilote (horloge monotone,
   flottant). Le temps **n'est pas suspendu** pendant les pauses (les animations « sautent » à la reprise).
3. Mise à jour audio : capture spectrale (§13) puis détecteur (§12).
4. Mise à jour média (§16).
5. Mise à jour des entrées (§10).
6. Traitement des événements du pilote + rendu (§8.2), incrément du compteur d'images.
7. Demande de fermeture du pilote → arrêt.
8. Détection plein écran (§9) ; si oui et pas d'arrêt demandé → entrée en pause, fin d'itération.
9. Avancement des playlists (§17).
10. Capture d'écran éventuelle (§18).

### 8.2 Cadence

- Plafond : durée minimale d'image = 1/FPS (valeur figée au premier appel). Après le travail d'une itération, si la durée
  écoulée est inférieure, sommeil du reliquat (résolution microseconde).
- Fenêtre et X11 : chaque itération rend toutes les sorties, puis échange de tampons, traitement des événements fenêtre.
- Wayland : la première itération rend une fois chaque sortie (amorçage) ; ensuite chaque itération **bloque** jusqu'à
  au moins un événement Wayland, et chaque sortie est rendue quand son rappel de trame arrive (cadence de
  rafraîchissement du compositeur, une sortie masquée/éteinte cesse naturellement d'être rendue). Le compteur d'images
  s'incrémente par itération, pas par sortie.
- Un même fond partagé (span) n'est rendu qu'une fois par valeur du compteur d'images.
- Le moteur web reçoit une fréquence hors écran = max(60, FPS) (le moteur de référence plafonne lui-même à 60).

---

## 9. Détection de plein écran et pause

### 9.1 Activation

- `--no-fullscreen-pause` → détecteur neutre (jamais plein écran). Ce détecteur neutre est aussi utilisé si la session
  n'est ni `x11` ni `wayland`.
- Le détecteur est consulté une fois par itération par la boucle (§8.1) et une fois par la détection audio (§12).

### 9.2 X11

- Au démarrage : liste des rectangles (x, y, l, h) des CRTC de **toutes** les sorties connectées actives (pas seulement
  celles qui portent un fond). XRandR absent → avertissement, détection inopérante.
- Requête : parcourir les enfants directs de la racine ; ignorer notre fenêtre et son parent ; ignorer les fenêtres non
  visibles (non « viewable ») ; si la géométrie (x, y, l, h) d'une fenêtre est **exactement égale** à celle d'un CRTC →
  plein écran.
- Aucune exception X n'est fatale ; une requête qui échoue répond « pas de plein écran ».

### 9.3 Wayland

- Connexion Wayland **séparée** ; liaison à `zwlr_foreign_toplevel_manager_v1` (version 3). Absent → message
  « détection plein écran non supportée par le compositeur », détecteur toujours négatif.
- Pour chaque fenêtre de premier niveau : suivre l'app_id, et à chaque lot d'états (événement « done ») les drapeaux
  *fullscreen* et *activated* en attente → courants.
- Fenêtre **pertinente** si : plein écran ET (option « seulement si actif » désactivée OU fenêtre activée) ET aucun motif
  de la liste d'exclusion (motifs vides ignorés) n'est une sous-chaîne de l'app_id, comparaison insensible à la casse
  ASCII. Une fenêtre sans app_id n'est jamais exclue.
- Un compteur est incrémenté/décrémenté lors des transitions pertinent/non pertinent à chaque « done », et décrémenté à la
  fermeture d'une fenêtre comptée ; un sous-dépassement est journalisé et ignoré.
- Requête : un aller-retour synchrone avec le compositeur, puis « plein écran » ⇔ compteur > 0.
- La sortie sur laquelle se trouve la fenêtre n'est pas prise en compte : une fenêtre plein écran sur n'importe quelle
  sortie met en pause tous les fonds.

### 9.4 Effets de la pause

- Entrée : tous les fonds reçoivent « pause » (la vidéo est mise en pause dans le lecteur ; les scènes et pages web ne
  sont simplement plus mises à jour) ; l'instant de début de pause est mémorisé.
- Pendant la pause, chaque itération : courte attente puis nouvelle requête au détecteur ; aucun rendu, aucune mise à jour
  d'entrée/audio/média, aucun traitement d'événements de fenêtre/Wayland. *[défaut amont]* l'attente est de 250 **µs**
  (quasi boucle active) (QO-7).
- Le son des scènes est coupé pendant le plein écran uniquement si l'automute est actif (§12) ; avec `--noautomute`,
  les sons de scène **continuent** pendant la pause.
- Sortie de pause : « reprise » envoyée aux fonds ; pour chaque playlist dont `updateonpause` est faux, les échéances sont
  décalées de la durée de la pause ; aucune image n'est rendue dans cette itération-là.

---

## 10. Entrées souris

### 10.1 Modèle

L'état souris exposé au rendu = position en pixels **physiques, convention GL** (Y vers le haut, relatif à l'origine de
la surface de rendu) + état bouton gauche + état bouton droit (relâché/enfoncé). Molette, bouton du milieu et clavier ne
sont pas gérés.

### 10.2 Acquisition par backend

- **Fenêtre et X11** (une mise à jour par itération) :
  - position du curseur relative à la fenêtre GL (en X11 bureau, la fenêtre est cachée et couvre l'écran X : la position
    vaut donc les coordonnées racine, y compris au-dessus des autres fenêtres) ;
  - Y_GL = hauteur du tampon − Y_écran (en X11 bureau, hauteur du tampon = hauteur totale de l'écran X) ;
  - boutons : état courant lu sur la fenêtre. En X11 bureau, la fenêtre cachée ne reçoit pas d'événements de bouton :
    les clics ne sont en pratique jamais vus (QO-8).
- **Wayland** (événementiel, par sortie) :
  - les événements n'arrivent que lorsque le pointeur survole la surface de fond (donc le bureau visible, pas les
    fenêtres) ; « entrée » mémorise la sortie qui a le focus ; « sortie » est ignorée (le focus et la dernière position
    restent) ;
  - mouvement (x, y) en coordonnées logiques de surface : position de la sortie focalisée ← (x·échelle,
    (hauteur_logique − y)·échelle) ;
  - boutons gauche/droit : pressé → enfoncé, relâché → relâché, enregistrés sur la sortie focalisée ;
  - position fournie au rendu d'une sortie S : si S est la sortie focalisée → sa dernière position ; sinon sa dernière
    position si elle est non nulle ; sinon le **centre** de S ; état des boutons = celui de S. Hors rendu → (0,0)
    et relâché.
  - *[défaut amont]* sous Hyprland, la référence interroge périodiquement (≤ 1 fois / 16 ms) la position globale du
    curseur via le socket IPC de Hyprland, mais n'utilise jamais le résultat : aucun effet observable, ne pas reproduire
    (QO-9).

### 10.3 Projection vers les coordonnées de scène

Pour une scène rendue sur le viewport (vx, vy, vl, vh) — pour un groupe span : (0, 0, largeur, hauteur) de la boîte
englobante — avec la position GL (px, py) et les UV courants (u0, u1, v0, v1) :

- mx = borne((px − vx)/vl, 0, 1) ; ny = borne((py − vy)/vh, 0, 1)
- **position normalisée** (exposée aux scripts comme position écran du curseur) :
  (u0 + mx·(u1 − u0), v0 + ny·(v1 − v0))
- **position pointeur** (uniforme shader `g_PointerPosition`, et entrée de la parallaxe) :
  (u0 + mx·(u1 − u0), v0 + (1 − ny)·(v1 − v0))
- `g_PointerPositionLast` = position pointeur de l'image précédente.
- Convention cible : en fenêtre et Wayland (vflip vrai, v0 = 0, v1 = 1), le pointeur vaut (0,0) en haut à gauche de la
  région visible du fond et (1,1) en bas à droite ; les UV de recadrage/bandes sont pris en compte. En X11 bureau, la
  combinaison de la relecture inversée et de la conversion Y semble inverser l'axe vertical (QO-10).

Parallaxe (si activée dans la caméra de la scène et sans `--disable-parallax`), à chaque image de scène :
retard = borne(délai_parallaxe · (g_Time − g_TimeLast), 0, 1) ;
déplacement ← mélange(déplacement, (pointeur − (0.5, 0.5)) · quantité · influence_souris, retard)
(mélange(a, b, t) = a + (b − a)·t). Les trois paramètres viennent de la scène (spec scène).

Les clics ne sont consommés **que** par les fonds web (§15.4) : les scènes ne réagissent pas aux clics dans la
référence (l'état « bouton gauche enfoncé » exposé aux scripts vaut toujours faux).

### 10.4 Projection vers une page web

Avec le viewport (vx, vy, vl, vh) et la position GL (px, py) :

- x_web = borne(ent(px − vx), 0, vl)
- y_web = vh − borne(ent(py − vy), 0, vh)  (origine en haut)
- Événement « mouvement » envoyé **à chaque image** de la page.
- Si l'état d'un bouton (gauche ou droit) a changé depuis l'image précédente : événement clic à la position courante,
  « bouton relevé » si l'état est relâché, nombre de clics = 1.

### 10.5 `--disable-mouse`

- Position forcée à (0,0) (fenêtre/X11 : les états de boutons ne sont plus mis à jour ; Wayland : région d'entrée vide).
- Conséquence visible conservée : le pointeur de scène vaut le coin (u0, v1) — pour les UV standard (0, 1), soit le coin
  bas gauche — et la parallaxe se décale vers ce coin, sauf `--disable-parallax` (QO-11).

---

## 11. Audio : lecture des sons de scène

### 11.1 Périphérique et mixage

- `--silent` : aucun périphérique n'est ouvert, aucun son de scène n'est chargé (message informatif).
- Sinon : ouverture du périphérique de sortie par défaut, demandé en 48 000 Hz, flottant 32 bits, stéréo, tampon de
  4096 trames ; le système peut imposer un autre format, qui est alors adopté. Échec d'initialisation → erreur journalisée
  et poursuite **sans son**.
- Chaque flux est décodé (tout format audio courant) et rééchantillonné au format du périphérique (mono, stéréo, ou
  « surround » pour > 2 canaux).
- Rappel de mixage : tampon de sortie mis à zéro ; si le détecteur indique « une autre application joue » (§12), on
  rend du silence **sans consommer** les flux (les sons sont donc suspendus, pas avancés) ; sinon chaque flux actif et non
  vide est ajouté : sortie ← borne(sortie + échantillon · volume/128, −1, 1) pour le format flottant (addition saturée
  équivalente pour les autres formats). volume = valeur CLI bornée [0, 128] (défaut 15 → gain ≈ 0,117).
- Un flux dont la file est momentanément vide est sauté pour ce rappel (et son lecteur est réveillé).
- Une erreur de décodage produit 1024 octets de silence.

### 11.2 Objets son (`sound` dans une scène)

Champs JSON utilisés :

- `sound` : tableau de chemins (dans l'espace de fichiers du fond, §3.4) ;
- `playbackmode` : chaîne ; `"loop"` → répétition infinie ; toute autre valeur ou absence → lecture unique.

Comportement :

- **Tous** les fichiers du tableau démarrent simultanément, dès la création de la scène (pas d'enchaînement, pas de tirage
  aléatoire).
- Ignorés par la référence : `volume`, `mintime`, `maxtime`, `muteineditor` et toute autre clé (QO-12).
- Lecture unique : à la fin, silence définitif (aucune relance).
- Boucle : retour au début dès la fin de fichier.
- Lecture anticipée : chaque flux pré-lit des paquets jusqu'à ~5 Mio en file, ou > 25 paquets et > 1 s de durée en file,
  puis attend (≈ 10 ms) — valeurs indicatives, seule la continuité sans coupure est exigée.
- *[défaut amont]* l'état de décodage partagé entre flux provoque des artefacts quand plusieurs sons jouent ; ne pas
  reproduire.

---

## 12. Audio : détection « une autre application joue » (automute)

### 12.1 Règles

- `--noautomute` : détecteur neutre, toujours « rien ne joue » (y compris en plein écran).
- Sinon, à chaque mise à jour (une par itération non en pause, plus deux au démarrage) :
  1. si l'automute est désactivé dans les réglages → « rien ne joue » ;
  2. si le détecteur plein écran répond oui → « joue » (le son est coupé pendant le plein écran) ;
  3. sinon « joue » ⇔ il existe, sur le serveur son (protocole PulseAudio, y compris PipeWire-pulse), au moins un flux
     de lecture (« sink input ») dont la propriété *identifiant de processus* existe, est **différente** du PID courant,
     et dont le volume moyen n'est pas « muet » (volume nul).
     - L'état « en pause » (corked) et le drapeau de sourdine du flux ne sont **pas** examinés (QO-13).
     - Un flux sans PID n'est pas compté.
- Effets : sons de scène suspendus (§11.1) ; fonds vidéo mis en sourdine / démutés au changement d'état (si le son est
  activé) ; fonds web non affectés.

### 12.2 Robustesse (exigée et testée, §21)

- La construction du détecteur ne doit jamais bloquer (connexion asynchrone).
- Événements du serveur traités sans attente, au plus 32 par itération.
- Délai de connexion : 2 s ; au-delà, ou état en échec → connexion abandonnée.
- Nouvelle tentative au plus toutes les 5 s ; la perte du serveur n'est journalisée qu'une fois ; message de reprise
  quand il revient.
- Une requête de liste des flux ne doit pas retenir une image plus de 100 ms ; dépassement → requête annulée et
  connexion considérée perdue.
- Sans serveur, l'état reste « rien ne joue » (sauf règle plein écran).

---

## 13. Audio : capture et analyse spectrale (audio-réactif)

### 13.1 Activation

- Activée si **au moins un** fond chargé au démarrage déclare `general.supportsaudioprocessing = true` **et**
  `--no-audio-processing` est absent. Sinon, analyseur neutre : tous les tableaux restent à 0.
- Décision prise au démarrage seulement (un fond audio-réactif chargé plus tard par playlist n'active pas la capture).
- *[défaut amont]* si le serveur son est injoignable, la construction de la capture attend indéfiniment (QO-14) ; Lumen
  DEVRAIT se comporter comme le détecteur (§12.2) et laisser les tableaux à 0.

### 13.2 Source

- Source = moniteur (« monitor ») du **puits par défaut** du serveur son (nom du puits par défaut + `.monitor`) ; donc tout
  ce que la machine joue, y compris le son du fond lui-même.
- Format de capture : PCM non signé 8 bits, mono, 44 100 Hz.
- Latence demandée : fragments ≈ 10 % d'une seconde de données (4 410 octets ≈ 100 ms) ; tampon maximal = fragment
  + 7,5 s de données.
- Abonnement aux changements de puits/sources : à chaque événement, si le flux actuel enregistre déjà le bon moniteur
  (ou est en cours de connexion vers lui), rien ; sinon le flux est déconnecté et recréé vers le nouveau moniteur par
  défaut (suit le changement de périphérique de sortie).

### 13.3 Fenêtre d'analyse

- Fenêtre de N = 1024 échantillons.
- Accumulation : les octets reçus remplissent la fenêtre en cours. Quand un paquet complète la fenêtre :
  - s'il reste au moins une fenêtre complète supplémentaire dans le paquet, la fenêtre prête devient **les 1024 derniers
    octets du paquet** et l'accumulation repart de zéro ;
  - sinon la fenêtre complétée devient la fenêtre prête et les octets excédentaires du paquet sont **jetés**.
  - Une fenêtre prête lève un drapeau « nouvelle trame ».
- Les « trous » signalés par le serveur sont ignorés.

### 13.4 Calcul (au plus une fois par itération, seulement si « nouvelle trame »)

1. Conversion : x[n] = (octet[n] − 128)/128, n = 0..1023 (**aucun fenêtrage** : fenêtre rectangulaire).
2. FFT réelle non normalisée de taille 1024 : X[k] = Σ x[n]·e^(−2iπkn/1024), k = 0..512.
3. Pour b = 0..63 :
   - k = 2·b (seuls les bins pairs 0..126 sont utilisés, ≈ 0 à 5,4 kHz, pas de 43,07 Hz) ;
   - p = Re(X[k])² + Im(X[k])² ;
   - L = 0,35·log10(p) si p > 0, sinon 0 (L peut être **négatif**) ;
   - cible64[b] = min(1, L·(2 − e^(0,5 − b/63))) ;
   - cible32[⌊b/2⌋] = min(1, L·(2 − e^(0,5 − b/31))) ;
   - cible16[⌊b/4⌋] = min(1, L·(2 − e^(0,5 − b/15))).
   Comme chaque case de 32/16 est écrite plusieurs fois dans l'ordre croissant de b, la valeur finale est celle du dernier
   b : cible32[j] utilise b = 2j+1 ; cible16[j] utilise b = 4j+3 (donc les bins k = 4j+2 et 8j+6).
4. Aucune borne inférieure : les valeurs peuvent être négatives ; borne supérieure 1.

Pondération (information) : facteur 2 − e^(0,5 − b/D) ≈ 0,351 pour b = 0, croissant vers 2 ; il atténue les graves.

### 13.5 Lissage et cadence

- À **chaque** mise à jour (une par itération de boucle non en pause, plus deux au démarrage), avant l'éventuel nouveau
  calcul : pour chaque case i, valeur[i] se rapproche de cible[i] d'au plus 0,3 (limitation de pente linéaire :
  si |cible − valeur| ≤ 0,3 alors valeur = cible, sinon valeur += signe(cible − valeur)·0,3).
- Le lissage dépend donc du FPS. Avec des fragments de ~100 ms, les cibles changent ≈ 10 fois par seconde.

### 13.6 Exposition

- Uniformes shader : `g_AudioSpectrum16Left`, `g_AudioSpectrum16Right` (16 flottants), `g_AudioSpectrum32Left`,
  `g_AudioSpectrum32Right` (32), `g_AudioSpectrum64Left`, `g_AudioSpectrum64Right` (64).
- **Gauche = droite** : capture mono, les deux canaux reçoivent exactement les mêmes valeurs.
- Aucune transmission aux pages web (§15.5) ni aux scripts de scène dans la référence.

---

## 14. Lecture vidéo

### 14.1 Fonds vidéo (`type: video`)

- Fichier = `file` de `project.json`, résolu en chemin physique via l'espace de fichiers du fond.
- Rendu dans l'image hors écran du fond, sans inversion verticale par le lecteur (l'inversion est gérée par la recopie,
  §4.5).
- Taille du fond = taille d'**affichage** du flux (tenant compte de l'aspect/rotation), connue au premier événement de
  reconfiguration vidéo ; avant cela, petite taille provisoire. Chaque reconfiguration redimensionne l'image hors écran.
- Lecture **en boucle infinie**, démarrage immédiat.
- Options de lecture équivalentes à reproduire :
  - décodage matériel selon `--hwdec` : `auto` (meilleur disponible), `nvdec-copy` (NVDEC avec recopie en mémoire
    système), `no` (logiciel) ;
  - aucune configuration utilisateur du lecteur n'est chargée ;
  - profil « rapide » (qualité de mise à l'échelle interne réduite au profit des performances) ;
  - sortie image 8 bits RGBA ; pas de curseur, pas d'entrées clavier du lecteur ;
  - le rendu d'une image ne bloque jamais en attendant l'horodatage cible (plusieurs sorties sur un seul fil ne doivent
    pas se synchroniser sur la grille de la vidéo) ;
  - le lecteur affiche ses messages d'état sur le terminal (niveau « status » en version optimisée).
- Volume : si le son est activé, volume_lecteur = volume_CLI·100/128 (échelle 0–100 du lecteur de référence, dont la
  courbe de gain est cubique : gain ≈ (volume_CLI/128)³) ; avec `--silent`, volume 0 (QO-15).
- Automute : à chaque image, si le son est activé et que l'état « une autre application joue » diffère de l'état de
  sourdine courant, bascule de sourdine.
- Pause plein écran : pause/reprise du lecteur.
- Le son vidéo est joué directement par le lecteur (pas par le mixeur des sons de scène).

### 14.2 Textures vidéo dans les scènes

- Une texture marquée vidéo (MP4 embarqué, cf. spec des textures) est jouée depuis la mémoire (flux en lecture seule avec
  positionnement), toujours **muette**, volume 0, en mode « non cadencé » (le lecteur ne s'aligne pas sur l'horloge,
  QO-16), en boucle infinie.
- La lecture ne démarre que lorsqu'au moins un objet utilise la texture (compteur d'usage) et s'arrête quand plus aucun
  ne l'utilise ; elle peut redémarrer ensuite depuis le début.
- La texture prend la taille d'affichage de la vidéo dès qu'elle est connue.

---

## 15. Navigateur web (fonds `type: web`)

### 15.1 Initialisation

- Le moteur web n'est initialisé que si au moins un fond web est chargé (au démarrage ou plus tard par playlist).
- Profil navigateur : dossier neuf nommé par un UUID v4 aléatoire dans le répertoire temporaire du système, à chaque
  lancement (jamais nettoyé par la référence, QO-17).
- Rendu hors écran (sans fenêtre) activé ; bac à sable désactivé selon la configuration de compilation.
- Échec d'initialisation → fatal.

### 15.2 Chargement de la page et protocole de fichiers

- Un schéma d'URL privé par fond : `wp` + identifiant du fond (§3.5), ex. `wp2317494988`, `wp-1`.
- Schéma déclaré « standard », « sécurisé » et autorisé pour `fetch`.
- URL initiale : `wp<id>://root/<file>`.
- Résolution d'une requête : l'hôte est ignoré ; le chemin d'URL, sans son `/` initial, est cherché dans l'espace de
  fichiers du fond (§3.4 — donc aussi dans les assets et le répertoire courant). La chaîne de requête et le fragment sont
  ignorés. Le chemin n'est **pas** décodé (`%20` n'est pas converti, QO-18).
- Réponse trouvée : statut 200, type MIME déduit de l'extension (table MIME usuelle), à défaut un type binaire générique
  (la référence envoie une variante mal orthographiée de `application/octet-stream`), longueur inconnue (diffusion
  jusqu'à fin de fichier).
- Non trouvée : statut 404 avec erreur « fichier introuvable ».

### 15.3 Configuration comportementale du moteur

À reproduire par équivalence fonctionnelle :

- sécurité web désactivée (pas de restriction d'origine croisée entre le schéma privé, `file:` ou le réseau) ;
- isolation de site / par origine désactivée ;
- lecture automatique des médias **sans geste utilisateur** ;
- aucun ralentissement des minuteries ni suspension des médias pour arrière-plan / occlusion ;
- pas de gestion des touches multimédia matérielles ;
- pas de cache disque des shaders, pas de rapport de plantage, pas d'expérimentations de terrain ;
- connexions de débogage distant acceptées depuis n'importe quelle origine (QO-19).

### 15.4 Rendu hors écran

- Taille de vue = taille du viewport (pixels physiques, facteur d'échelle d'appareil 1). Initialement 16×17, ajustée au
  premier rendu, puis à chaque changement de taille du viewport (notification de redimensionnement au moteur).
- Chaque image peinte (BGRA, image complète, sans exploiter les rectangles modifiés) est téléversée dans l'image hors
  écran du fond, puis recopiée vers la sortie selon §4.5 (orientation finale : droite).
- La boucle de messages du moteur est pompée une fois par image du fond ; aucun effacement préalable (évite le
  scintillement quand aucune peinture n'a eu lieu dans l'image).
- Pendant la pause plein écran, la page n'est plus pompée (gelée).
- Souris : §10.4. Pas de clavier, pas de molette.

### 15.5 API JavaScript

**La référence n'injecte aucune API JavaScript.** En particulier :

- `window.wallpaperPropertyListener` n'est jamais appelé (ni `applyUserProperties`, ni `applyGeneralProperties`, ni
  `setPaused`, ni les rappels de dossiers utilisateur) ;
- `window.wallpaperRegisterAudioListener` n'existe pas (aucune donnée audio n'est transmise) ;
- les API média (`wallpaperRegisterMediaStatusListener`, `…PropertiesListener`, `…ThumbnailListener`,
  `…PlaybackListener`, `…TimelineListener`) et `wallpaperRequestRandomFileForProperty` n'existent pas ;
- les valeurs de `--set-property` ne sont pas transmises.

Une page qui appelle ces fonctions lève donc une erreur JS (non fatale pour l'application). La décision de dépasser la
parité est la QO-20 ; si Lumen l'implémente, le contrat public de Wallpaper Engine à respecter est résumé ici
(documentation publique de Wallpaper Engine, à revalider) :

| API | Contrat public |
|---|---|
| `window.wallpaperPropertyListener.applyUserProperties(props)` | objet clé → `{ value }` ; appelé au chargement avec toutes les propriétés puis à chaque changement avec les seules propriétés modifiées. Couleur = chaîne `"r g b"` (flottants 0–1 séparés par des espaces) ; booléen ; nombre (slider) ; chaîne/valeur (combo, texte) ; chemin (fichier/dossier). |
| `…applyGeneralProperties(props)` | propriétés générales, notamment `fps`. |
| `…setPaused(isPaused)` | notification pause/reprise. |
| `…userDirectoryFilesAddedOrChanged(name, files)` / `…userDirectoryFilesRemoved(name, files)` | propriétés de type dossier. |
| `window.wallpaperRegisterAudioListener(cb)` | `cb(tableau)` de 128 flottants : 0–63 canal gauche, 64–127 canal droit, graves en premier, ≈ 0–1 (peut dépasser 1), ≈ 30 appels/s. |
| `window.wallpaperRegisterMedia…Listener(cb)` | état (activé), propriétés (titre, artiste, album…), vignette, lecture (arrêté/joue/pause), chronologie (position, durée). |

Si cette extension est retenue, la source audio naturelle est l'analyse du §13 étendue à 64 bandes par canal.

### 15.6 Audio des pages web

- Le son des pages est émis directement par le moteur web : **ni `--volume`, ni `--silent`, ni l'automute ne
  s'appliquent** dans la référence (QO-21).
- Le moteur de référence joue son audio depuis un processus auxiliaire : son flux a donc un PID différent et peut être
  compté par le détecteur §12 comme « une autre application », coupant les sons des scènes affichées sur d'autres
  sorties.

### 15.7 Processus auxiliaires

- La référence relance son propre exécutable comme processus auxiliaires du navigateur, en ajoutant tous les arguments
  d'origine (afin que chaque auxiliaire recharge les mêmes fonds et déclare les mêmes schémas). Les auxiliaires de type
  « zygote » et « utility » n'écrivent aucun journal. Ce mécanisme est un détail d'implémentation : Lumen peut utiliser
  un autre moteur, à condition que les comportements des §15.2–15.6 soient conservés.

---

## 16. Médias (lecteur en cours, MPRIS)

### 16.1 Connexion

- Bus de session D-Bus ; échec → **fatal** dans la référence (QO-22).
- Abonnement à tous les signaux « propriétés modifiées » ; seuls ceux de l'interface `org.mpris.MediaPlayer2.Player` sont
  traités, **quel que soit l'émetteur** (pas de filtrage par lecteur courant).

### 16.2 Détection initiale

- Lister les noms du bus ; garder ceux qui commencent par `org.mpris.MediaPlayer2.`, dans l'ordre de la liste.
- Pour chacun : lire `PlaybackStatus`. Le premier dont l'état est `Playing` devient le lecteur courant et la recherche
  s'arrête. Chaque état lu met à jour l'état de lecture courant. Un échec de lecture interrompt la recherche.
- Si un lecteur courant existe : lecture de `Metadata` (§16.3).

### 16.3 Interprétation

- `Metadata` : `xesam:title` (chaîne) ; `xesam:artist` (tableau : **premier élément** seulement) ; `xesam:album` ;
  `mpris:artUrl` ; `mpris:length` (entier, microsecondes). Les changements de titre, artiste ou durée déclenchent
  l'événement « métadonnées » ; les changements d'album ou d'URL de pochette déclenchent l'événement « pochette ». Une URL
  vide efface la pochette.
- `PlaybackStatus` : `Playing` → joue (et l'émetteur devient le lecteur courant), `Paused` → pause, autre → arrêté ;
  un changement déclenche « métadonnées ».
- Position : `Position` (µs) du lecteur courant ; un changement déclenche « métadonnées ».
- L'indicateur « disponible » n'est jamais positionné à vrai.

### 16.4 Cadence et consommateurs

- À chaque itération : traitement non bloquant des messages reçus. La référence prévoit une interrogation de `Position`
  toutes les 2 s, mais l'échéance n'étant jamais avancée, la requête (bloquante) part **à chaque itération** dès qu'un
  lecteur courant existe *[défaut amont]* (QO-23).
- Consommateurs :
  - scripts de scène (événements média : spec des scripts) ;
  - textures spéciales `$mediaThumbnail` (pochette courante) et `$mediaPreviousThumbnail` (précédente) : à chaque
    événement « pochette », si la pochette courante est prête, elle est copiée dans « précédente », puis la nouvelle est
    chargée depuis l'URL. Seules les URL `file://` sont acceptées ; le fichier doit exister et être un fichier régulier ;
    sinon la pochette n'est pas chargée (erreur non fatale).

---

## 17. Playlists (configuration de Wallpaper Engine)

### 17.1 Source

- Fichier `<installation WE>/config.json` (§3.2). Installation introuvable ou fichier impossible à ouvrir → **fatal** ;
  JSON invalide → erreur journalisée, aucune playlist chargée (donc échec fatal du `--playlist` demandé).
- Lu au premier `--playlist`, une seule fois.
- Section racine `steamuser` obligatoire (absente → fatal).
- Playlists collectées :
  1. `steamuser.general.playlists[]` : nom = champ `name` ;
  2. `steamuser.wallpaperconfig.selectedwallpapers.<clé>.playlist` : nom = champ `name`, à défaut la `<clé>`.
  Une playlist de même nom enregistrée plus tard **remplace** la précédente (le second groupe l'emporte).
- Une playlist sans élément utilisable est ignorée (erreur journalisée) ; sans nom ni repli → ignorée.
- `--playlist NOM` inconnu → fatal, le message liste les noms disponibles (séparés par des virgules).

### 17.2 Éléments

Chaque élément est une chaîne de chemin (au format Windows de Wallpaper Engine) normalisée ainsi :

1. retrait d'un préfixe `\\?\` ;
2. `\` → `/` ;
3. si le 2ᵉ caractère est `:`, retrait des deux premiers (lettre de lecteur) ;
4. ajout d'un `/` initial s'il manque ;
5. normalisation lexicale (`.`/`..`) ;
6. si le résultat est un fichier régulier, on prend son dossier parent.

Un élément vide, non-chaîne ou inexistant est ignoré (journalisé). (Correspondance Windows → Linux : QO-24.)

### 17.3 Réglages (`settings`)

| Clé | Défaut | Sens |
|---|---|---|
| `delay` | 60 | minutes entre changements (minimum effectif 1) |
| `mode` | `"timer"` | seul `timer` déclenche des changements ; toute autre valeur fige la playlist |
| `order` | `"sequential"` | `random` → ordre mélangé |
| `updateonpause` | faux | si faux, le temps de pause plein écran est exclu du minuteur |
| `videosequence` | faux | lu mais sans effet |

### 17.4 Fonctionnement

- Ordre initial : 0..n−1, mélangé si `random` (générateur pseudo-aléatoire initialisé aléatoirement).
- Position initiale : index (dans l'ordre) de l'élément actuellement affiché (fenêtre : premier élément ; sortie : fond
  courant de la sortie), sinon 0. Première échéance = maintenant + délai.
- À chaque itération : pour chaque playlist en mode `timer` comptant ≥ 2 éléments dont l'échéance est atteinte :
  1. position ← (position + 1) mod n ; si on revient à 0 en mode `random`, nouveau mélange ;
  2. sauter les éléments marqués en échec (au plus n essais) ; tous en échec → on garde le fond actuel, nouvelle échéance ;
  3. pré-vérification du candidat : `project.json` lisible avec `type` et `file` ; échec → marqué en échec, on prend le
     suivant non marqué ;
  4. chargement du fond, application des surcharges de propriétés, initialisation du web si nécessaire, remplacement du
     moteur de rendu de la sortie (avec ses réglages d'échelle/bord) ; la capture d'écran est réarmée (§18.2) ;
  5. échec de chargement → élément marqué en échec, position conservée ;
  6. nouvelle échéance = maintenant + délai (dans tous les cas).
- Les éléments en échec le restent pour toute la session.
- *[défaut amont]* une playlist attachée à un groupe span ne fonctionne pas (clé synthétique sans sortie) (QO-25).

---

## 18. Captures d'écran

### 18.1 Validation (au démarrage)

- Le chemin doit avoir une extension parmi `.bmp`, `.png`, `.jpg`, `.jpeg` (comparaison **sensible à la casse**) ;
  sinon fatal (« format indéterminable » / « extension inconnue »).

### 18.2 Déclenchement

- Armement : à chaque chargement de fond (démarrage et changements de playlist), échéance = délai (0..5) + compteur
  d'images courant (0 au démarrage).
- Prise : en fin d'itération (sauf itération restée en pause), si capture demandée, non encore prise, et compteur
  d'images ≥ échéance.
- Une seule capture par armement ; l'application **continue de tourner** ensuite.
- L'encodage et l'écriture se font en arrière-plan (ne pas bloquer le rendu) ; un arrêt immédiat peut perdre le fichier.

### 18.3 Contenu

- Image RGB 8 bits, dimensions = taille totale de la sortie (fenêtre : taille de la fenêtre ; X11 : écran X entier ;
  Wayland : somme des largeurs × max des hauteurs, en pixels physiques).
- Fond noir, puis pour chaque sortie (ordre alphabétique) : lecture de l'image hors écran du **fond** (à sa résolution
  propre l_f × h_f, après rendu complet), puis rééchantillonnage au **plus proche voisin** vers la taille du viewport
  (vl × vh) : pour chaque pixel (x, y), u = u_début + (x/vl)·(u_fin − u_début), v = v_début + (y/vh)·(v_fin − v_début),
  source = (borne(ent(u·l_f), 0, l_f−1), borne(ent(v·h_f), 0, h_f−1)). Les pixels hors image sont ignorés.
- Placement horizontal : Wayland → les sorties sont posées de gauche à droite, dans l'ordre alphabétique, à
  x = somme des largeurs précédentes, y = 0. Fenêtre → x = 0. X11 → *[défaut amont]* toutes les sorties sont posées à
  x = 0, y = 0 (superposition) (QO-26).
- Orientation : l'image produite DOIT être droite (haut de l'image = haut de l'écran).
- Encodage selon l'extension : BMP ; PNG ; JPEG qualité 100.
- Une erreur de lecture d'une sortie est journalisée et cette sortie est omise.
- Usage typique : génération de palette (pywal…).

---

## 19. Journalisation

- Deux canaux : informations → sortie standard ; erreurs → sortie d'erreur. Chaque message = une ligne.
- Messages de débogage : uniquement dans les constructions de débogage (et une variante « erreurs seulement » existe pour
  les messages de récupération X11).
- Le contenu exact des messages est libre, mais les informations suivantes DEVRAIENT être présentes : ligne « en cours
  d'exécution avec » (§1.1), dossier d'assets imposé, sortie trouvée (nom, position, taille), application d'une surcharge,
  son désactivé, détection plein écran non supportée, perte/reprise du serveur son, échec de capture audio, arrêt (signal
  ou pilote), « arrêt » final, fermeture du moteur web.
- Les erreurs graphiques de haute sévérité (constructions de débogage) sont journalisées avec une pile d'appels (§20).

## 20. Débogage

- `--dump-structure` : après la préparation, impression de la structure analysée de chaque fond rendu (format : spec des
  données).
- `--render-debug` (répétable) :

| Valeur | Effet |
|---|---|
| `base-only` | les images sont rendues sans effets ni passes de mélange de couleur supplémentaires |
| `no-solid-final` | les calques marqués « solides » ne sont pas dessinés dans la passe finale |
| `pass-log` | journalise chaque passe de rendu (objet, shader, cible, tailles) |
| `object=<id>` | ne rend que l'objet d'identifiant donné (le dernier gagne) |
| `skip-object=<id>` | ne rend pas cet objet (cumulable) |
| `skip-effect=<id>` | ignore cet effet (cumulable) |

- Constructions de débogage : sortie de débogage graphique synchrone ; pour chaque message de haute sévérité, journal
  d'erreur (message, type, identifiant) suivi d'une pile d'appels, une ligne par cadre, numérotée de N à 1 (décalage,
  fonction démanglée, module) ; étiquettes de groupes de débogage par sortie / scène pour les outils de capture GPU.
- Mode « démo » (option de compilation, optionnel pour Lumen) : FPS forcé à 30, capture et pause plein écran
  désactivées, un seul fond autorisé ; après FPS images de chauffe, enregistre 150 images (5 s) du fond dans
  `output.webm` (VP9) puis s'arrête.

---

## 21. Ce que testent les harnais de la référence (et tests de parité suggérés)

### 21.1 Tests existants

1. **Trivial** : un test toujours vrai (fumée du harnais).
2. **Propriétés** : une propriété booléenne sans valeur vaut faux ; une propriété de type `directory` est traitée comme
   une propriété « fichier » (son listage contient `<clé> - file`).
3. **Coordonnées souris** (arithmétique pure) :
   - fenêtre → GL : Y_GL = H − Y (H = 1080 : 0→1080, 1080→0, 540→540 ; idem avec H = 1440 et 600) ;
   - Wayland → GL : même formule ;
   - GL → normalisé : borne((y − vy)/vh, 0, 1) : 1080→1,0 ; 0→0,0 ; 540→0,5 ;
   - GL → web : vh − borne(ent(y − vy), 0, vh) : 1080→0 ; 0→1080 ; 540→540 ;
   - chaîne complète fenêtre → normalisé : haut → 1,0, bas → 0,0.
4. **Détecteur automute sans serveur** (serveur pointé vers un socket inexistant via la variable d'environnement du
   serveur son) : construction < 500 ms ; mises à jour toutes les ~10 ms pendant 1,5 s, chacune < 250 ms ; non connecté ;
   « rien ne joue ».
5. **Serveur muet** (socket qui accepte sans jamais répondre) : construction < 500 ms ; mises à jour pendant 3 s (couvre
   le délai de connexion de 2 s), chacune < 250 ms ; non connecté ; « rien ne joue ».
6. **Réglages sans serveur** : plein écran forcé → « joue » ; automute désactivé + plein écran → « rien ne joue ».
7. **Échafaudage de rendu** : pilote de test (fenêtre GL cachée, contexte de débogage, souris neutre à (0,0) relâchée,
   pas de limitation de FPS) — prévu pour des tests de rendu, non utilisé par des cas actuellement.

### 21.2 Tests de parité recommandés pour Lumen

- Analyse CLI : table d'entrées → réglages attendus (ciblage §2.3, exclusions, bornes volume/délai, `--set-property`
  sans `=`, ID vs chemin, `span:` synthétique, piège du `--scaling`).
- Résolution Steam : arborescence factice sous un `$HOME` temporaire (ordre des racines §3.1, assets §3.3).
- UV (§4.5) : tous les modes × (paysage/portrait/carré) × vflip, y compris troncatures entières ; tranches span (§4.4).
- Souris : formules §10.3/§10.4 avec UV recadrés ; `--disable-mouse` → pointeur (u0, v1).
- Spectre : signaux synthétiques (silence → 0 ; sinusoïde pure à la fréquence du bin 2b → pic sur la bande b ;
  saturation → 1 ; signal faible → valeurs négatives), écrasements 32/16 (b = 2j+1, 4j+3), lissage 0,3/mise à jour.
- Fenêtrage de capture (§13.3) : paquets de tailles variées → fenêtre retenue attendue.
- Pertinence plein écran Wayland (§9.3) : combinaisons fullscreen/activated/app_id/motifs, transitions et fermetures.
- Playlists : normalisation des chemins (§17.2), avancement, échecs, mélange au bouclage, décalage pause.
- Capture : validation d'extension sensible à la casse, dimensions et placement par backend, échantillonnage au plus
  proche.
- Détecteur automute : reprendre les tests 4–6 avec les mêmes bornes temporelles.

---

## 22. Portage Windows (hors référence)

La référence ne fonctionne que sous Linux. Pour Lumen sous Windows, les équivalents fonctionnels suivants sont proposés
(à valider, QO-27) : répertoires Steam via le registre et `libraryfolders.vdf` ; fond de bureau derrière les icônes
(fenêtre enfant du « WorkerW ») avec un viewport par moniteur (noms de moniteurs à définir pour `-r`) ; plein écran via
l'état de notification utilisateur ou la comparaison de la fenêtre au premier plan avec les moniteurs ; automute via les
sessions audio d'autres processus actives et non muettes ; capture spectrale via la boucle de rendu (loopback) du
périphérique par défaut, ramenée à 44,1 kHz mono 8 bits équivalent pour conserver l'échelle des valeurs ; médias via les
contrôles de transport système ; `--layer` sans effet.

---

## 23. Questions ouvertes

- **QO-1** `--fps 0` ou négatif : la référence ne valide pas (0 → durée infinie / comportement indéfini, négatif → pas
  de plafond). Proposer : rejet fatal si ≤ 0 ?
- **QO-2** Le piège de `--scaling`/`--clamp` (appliqué à la cible courante, défaut des suivantes seulement s'il précède
  tout `-r`) doit-il être reproduit tel quel ? La doc utilisateur de la référence mentionne aussi `--clamping`, qui
  n'existe pas (seul `--clamp` est accepté) : accepter un alias ?
- **QO-3** Lumen doit-il chercher aussi dans les bibliothèques Steam additionnelles (`libraryfolders.vdf`) ? (écart
  volontaire de parité).
- **QO-4** Identité visible : classe WM X11 « linux-wallpaperengine », titre « wallpaperengine », espace de noms
  layer-shell « linux-wallpaperengine ». Les conserver (compatibilité des règles de compositeur existantes) ou utiliser
  « lumen » ?
- **QO-5** Après fermeture d'une surface Wayland par le compositeur, faut-il la recréer (ex. reconnexion d'écran) ?
  La référence ne le fait pas.
- **QO-6** Un changement de mode d'une sortie Wayland remplace la taille logique par la taille physique (faux si
  échelle > 1). Reproduire ou corriger ?
- **QO-7** Attente de 250 µs en pause (quasi boucle active, coûteuse en CPU surtout en X11). Proposer 250 ms ?
- **QO-8** En X11 bureau, les clics ne sont jamais reçus (fenêtre cachée) et la position du curseur est suivie même
  au-dessus des fenêtres ; sous Wayland c'est l'inverse (seulement au-dessus du bureau). Uniformiser ?
- **QO-9** Interrogation IPC Hyprland sans effet : confirmer qu'elle est omise.
- **QO-10** Axe vertical du pointeur en X11 bureau probablement inversé par rapport à la fenêtre/Wayland ; à confirmer
  par mesure sur la référence avant de décider de reproduire.
- **QO-11** `--disable-mouse` fige le pointeur dans un coin et décale la parallaxe : reproduire, ou centrer le pointeur ?
  Idem pour les groupes span sous Wayland, où la position locale d'une sortie est rapportée à la boîte englobante
  entière (pointeur faux hors de la première sortie).
- **QO-12** Implémenter `volume`, `mintime`/`maxtime` et un mode de lecture séquentiel/aléatoire des objets son (connus de
  Wallpaper Engine mais ignorés par la référence) ?
- **QO-13** Un flux d'une autre application en pause (corked) ou en sourdine déclenche l'automute dans la référence.
  Reproduire ?
- **QO-14** Blocage infini de la capture spectrale sans serveur son : corriger (recommandé) ?
- **QO-15** Courbe de volume vidéo cubique (≈ (v/128)³) vs linéaire (v/128) pour les sons de scène : au défaut 15, la
  vidéo est ~70 fois plus faible que les sons de scène. Reproduire exactement ?
- **QO-16** Textures vidéo « non cadencées » : la référence semble avancer d'une image vidéo par rendu plutôt qu'au
  rythme réel ; mesure nécessaire pour fixer la vitesse attendue.
- **QO-17** Nettoyer le profil navigateur temporaire à la sortie ?
- **QO-18** Décoder les chemins URL (`%20`, UTF-8) dans le schéma privé ? Gérer les requêtes `Range` pour les médias ?
- **QO-19** Désactiver la sécurité web et autoriser le débogage distant depuis toute origine sont des choix risqués :
  les garder pour la parité ?
- **QO-20** Implémenter l'API JS de Wallpaper Engine (§15.5) au-delà de la parité ? Si oui : quelles propriétés
  transmettre, cadence et format audio (128 valeurs, échelle de §13 ou autre), `setPaused` en plein écran ?
- **QO-21** Appliquer `--volume`, `--silent` et l'automute à l'audio des pages web ?
- **QO-22** Rendre l'absence de D-Bus non fatale (recommandé, et indispensable sous Windows) ?
- **QO-23** Requête bloquante de position MPRIS à chaque image : appliquer l'intervalle prévu de 2 s ? Filtrer les
  signaux par lecteur courant ? Supporter les pochettes `http(s)://` ?
- **QO-24** Normalisation des éléments de playlist (lecteur retiré, `/` préfixé) : sous Linux, quand le `config.json`
  vient d'une installation Wine/Proton, faut-il plutôt mapper via le préfixe ? Sous Windows, garder les chemins natifs.
- **QO-25** Playlists sur groupe span et clé vide de `--bg` sans cible : corriger ?
- **QO-26** Capture X11 multi-écran superposée à x = 0 : corriger en plaçant chaque sortie à sa position CRTC ?
  (Recommandé : placement à la position réelle pour tous les backends.)
- **QO-27** Portage Windows (§22) : noms d'écran pour `-r`, mécanisme d'ancrage, sources audio/média ; à spécifier
  dans une spec dédiée.
- **QO-28** Plusieurs arguments positionnels : comportement de la référence non déterminé (ignoré ou erreur) ; fixer
  « erreur » ?
