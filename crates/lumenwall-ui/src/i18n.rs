//! English and French strings, compiled in. `{n}`-style placeholders are filled by
//! the caller with `str::replace`.

use crate::config::LangPref;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    En,
    Fr,
}

impl Lang {
    pub fn resolve(pref: LangPref) -> Lang {
        match pref {
            LangPref::En => Lang::En,
            LangPref::Fr => Lang::Fr,
            LangPref::System => match sys_locale::get_locale() {
                Some(l) if l.to_ascii_lowercase().starts_with("fr") => Lang::Fr,
                _ => Lang::En,
            },
        }
    }
}

macro_rules! strings {
    ($($key:ident => $en:literal, $fr:literal;)*) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum K { $($key),* }

        impl K {
            #[cfg(test)]
            pub const ALL: &'static [K] = &[$(K::$key),*];

            pub fn get(self, lang: Lang) -> &'static str {
                match lang {
                    Lang::En => match self { $(K::$key => $en),* },
                    Lang::Fr => match self { $(K::$key => $fr),* },
                }
            }
        }
    };
}

strings! {
    AppName => "Lumenwall", "Lumenwall";
    NavLibrary => "Library", "Bibliothèque";
    NavScreens => "Screens", "Écrans";
    NavWorkshop => "Workshop", "Workshop";
    WsSubtitle => "Community wallpapers for Wallpaper Engine, on Steam", "Fonds d'écran de la communauté Wallpaper Engine, sur Steam";
    WsOwnTitle => "Subscribing goes through Steam", "L'abonnement passe par Steam";
    WsOwnBody => "It opens the item in Steam and requires owning Wallpaper Engine. Steam downloads it; it then appears here and in your library by itself.",
        "Le bouton ouvre la fiche dans Steam et nécessite de posséder Wallpaper Engine. Steam télécharge le fond d'écran ; il apparaît ensuite ici et dans votre bibliothèque, tout seul.";
    WsOwnShort => "Requires owning Wallpaper Engine. Steam downloads; Lumenwall adds it here by itself.", "Nécessite de posséder Wallpaper Engine. Steam télécharge, Lumenwall l'ajoute ici tout seul.";
    WsBrowseSteam => "Browse in Steam", "Parcourir dans Steam";
    WsOpenBrowser => "Open in browser", "Ouvrir dans le navigateur";
    WsSearchHint => "Search the Workshop", "Rechercher dans le Workshop";
    WsTrend => "Trending", "Tendances";
    WsPopular => "Most popular", "Plus populaires";
    WsRecent => "Most recent", "Plus récents";
    WsResultsFor => "Results for “{q}”", "Résultats pour « {q} »";
    WsKeyTitle => "Browse the whole Workshop right here", "Parcourez tout le Workshop, ici même";
    WsKeyBody => "Lumenwall uses Steam's official Web API, which asks for your personal key (free, tied to your Steam account). Add it in Settings: it stays on this computer. Until then, browse in Steam; new subscriptions still show up here automatically.",
        "Lumenwall utilise l'API Web officielle de Steam, qui demande votre clé personnelle (gratuite, liée à votre compte Steam). Ajoutez-la dans les Réglages : elle reste sur cet ordinateur. En attendant, parcourez dans Steam ; vos nouveaux abonnements apparaissent quand même ici, automatiquement.";
    WsAddKey => "Add my key", "Ajouter ma clé";
    WsGetKey => "Get a key", "Obtenir une clé";
    WsNewTitle => "Just arrived", "Récemment ajoutés";
    WsInstalledTitle => "Your Workshop wallpapers", "Vos fonds d'écran du Workshop";
    WsInstalledNone => "No Workshop wallpaper installed yet.", "Aucun fond d'écran du Workshop installé pour l'instant.";
    Installed => "Installed", "Installé";
    NewBadge => "New", "Nouveau";
    NewWallpaper => "New wallpaper", "Nouveau fond d'écran";
    By => "by {name}", "par {name}";
    WsSubscribe => "Subscribe in Steam", "S'abonner dans Steam";
    WsShowInLibrary => "Show in library", "Voir dans la bibliothèque";
    WsRatings => "{n} ratings", "{n} évaluations";
    WsSubscribers => "Subscribers", "Abonnés";
    WsFavorites => "Favorites", "Favoris";
    WsSize => "Size", "Taille";
    WsUpdatedLabel => "Updated", "Mise à jour";
    WsAuthorProfile => "Author's profile", "Profil de l'auteur";
    WsScreenshots => "Screenshots", "Captures";
    WsLoading => "Loading the Workshop…", "Chargement du Workshop…";
    WsLoadMore => "Load more", "Charger plus";
    WsEnd => "You've seen everything.", "Vous avez tout vu.";
    WsOffline => "Steam can't be reached", "Steam est injoignable";
    WsOfflineBody => "Check your connection, then retry. What was already loaded stays available.", "Vérifiez votre connexion, puis réessayez. Ce qui est déjà chargé reste disponible.";
    WsQuota => "Steam asks to slow down", "Steam demande de ralentir";
    WsQuotaBody => "Too many requests in a short time. Wait a minute, then retry.", "Trop de requêtes en peu de temps. Patientez une minute, puis réessayez.";
    WsForbidden => "Steam refused the API key", "Steam a refusé la clé API";
    WsForbiddenBody => "Check the key in Settings: it may have been revoked.", "Vérifiez la clé dans les Réglages : elle a peut-être été révoquée.";
    WsError => "Steam answered with an error", "Steam a répondu par une erreur";
    WsNoResults => "Nothing found", "Aucun résultat";
    WsNoResultsBody => "Try other words, or fewer filters.", "Essayez d'autres mots, ou moins de filtres.";
    Retry => "Retry", "Réessayer";
    WsTotal => "{n} wallpapers", "{n} fonds d'écran";
    SecWorkshop => "Steam Workshop", "Workshop Steam";
    ApiKey => "Steam Web API key", "Clé Steam Web API";
    ApiKeyHelp => "Personal and free. Lumenwall keeps it only on this computer, in a private file, and sends it only to api.steampowered.com. Without it, the Workshop page still opens Steam and shows your installed wallpapers.",
        "Personnelle et gratuite. Lumenwall la garde uniquement sur cet ordinateur, dans un fichier privé, et ne l'envoie qu'à api.steampowered.com. Sans elle, la page Workshop ouvre quand même Steam et affiche vos fonds installés.";
    ApiKeyPlaceholder => "Paste your key (32 hexadecimal characters)", "Collez votre clé (32 caractères hexadécimaux)";
    KeyStored => "Saved key: {k}", "Clé enregistrée : {k}";
    KeySaved => "Steam Web API key saved", "Clé Steam Web API enregistrée";
    KeyInvalid => "That is not a Steam Web API key (32 hexadecimal characters).", "Ce n'est pas une clé Steam Web API (32 caractères hexadécimaux).";
    Save => "Save", "Enregistrer";
    ShowKey => "Show", "Afficher";
    HideKey => "Hide", "Masquer";
    ShowQuestionable => "Show “Questionable” wallpapers", "Afficher les fonds « Questionable »";
    ShowMature => "Show “Mature” wallpapers", "Afficher les fonds « Mature »";
    ContentHelp => "Off by default: only “Everyone” wallpapers are listed.", "Désactivé par défaut : seuls les fonds « Everyone » sont listés.";
    NavSettings => "Settings", "Réglages";

    EngineRunning => "Engine running", "Moteur actif";
    EngineStopped => "Engine not running", "Moteur arrêté";
    EngineConnecting => "Connecting…", "Connexion…";
    EngineStarting => "Starting the engine…", "Démarrage du moteur…";
    EngineIncompatible => "Incompatible engine version", "Version du moteur incompatible";
    EngineMissing => "Engine not found", "Moteur introuvable";
    EngineNoAnswer => "The engine started but does not answer", "Le moteur a démarré mais ne répond pas";
    StartEngine => "Start engine", "Lancer le moteur";
    DemoBanner => "The engine is not running. You can browse your wallpapers; start the engine to apply them.",
        "Le moteur n'est pas lancé. Vous pouvez parcourir vos fonds d'écran ; lancez le moteur pour les appliquer.";
    EngineMissingHelp => "Lumenwall looks for the “lumenwall” program next to this app, then in PATH.",
        "Lumenwall cherche le programme « lumenwall » à côté de cette application, puis dans le PATH.";

    Pause => "Pause", "Pause";
    Resume => "Resume", "Reprendre";
    Mute => "Mute", "Couper le son";
    Unmute => "Unmute", "Rétablir le son";
    Volume => "Volume", "Volume";
    Paused => "Paused", "En pause";
    PausedFullscreen => "Paused: an app is fullscreen", "En pause : application en plein écran";
    PausedBattery => "Paused: on battery", "En pause : sur batterie";
    PausedLocked => "Paused: session locked", "En pause : session verrouillée";
    PausedHidden => "Paused: desktop hidden", "En pause : bureau masqué";
    Playing => "Playing", "En lecture";

    PerfNoData => "No data yet", "Pas encore de mesures";
    PerfEngineRam => "Engine RAM", "RAM moteur";
    PerfAppRam => "App RAM", "RAM appli";
    SortBy => "Sort", "Trier";
    KeySearch => "Search", "Rechercher";
    KeyNavigate => "Move through wallpapers", "Parcourir";
    KeyApply => "Apply", "Appliquer";
    KeyClose => "Clear, close", "Effacer, fermer";
    KeyEnter => "Enter", "Entrée";
    KeyEscape => "Esc", "Échap";
    EngineOfflineHint => "Browse freely; start the engine to apply wallpapers.", "Parcourez librement ; lancez le moteur pour appliquer.";
    DropOnScreen => "Drop on a screen", "Déposez sur un écran";
    DragHint => "Tip: double-click a wallpaper, or drag it onto a screen.", "Astuce : double-cliquez un fond d'écran, ou glissez-le sur un écran.";
    Details => "Details", "Détails";
    MoreText => "More", "Plus";
    LessText => "Less", "Moins";
    LibrarySubtitle => "{n} wallpapers ready to play", "{n} fonds d'écran prêts à jouer";
    NoMatchBody => "Try another word, or clear the filters.", "Essayez un autre mot, ou effacez les filtres.";
    ScreensSubtitle => "Click a screen to choose its wallpaper.", "Cliquez sur un écran pour choisir son fond d'écran.";
    SettingsSubtitle => "Lumenwall stays light: every option here can only lower its cost.", "Lumenwall reste léger : chaque option ici ne peut qu'en réduire le coût.";
    ApplyAgain => "Apply again", "Réappliquer";
    PreviewAnimHelp => "On hover is the lightest.", "« Au survol » est le plus léger.";
    Close => "Close", "Fermer";
    WinMinimize => "Minimize", "Réduire";
    WinMaximize => "Maximize", "Agrandir";
    WinRestore => "Restore", "Restaurer";
    Browse => "Browse…", "Parcourir…";

    SearchHint => "Search title, tag or id", "Rechercher un titre, un tag, un id";
    All => "All", "Tous";
    KindScene => "Scene", "Scène";
    KindVideo => "Video", "Vidéo";
    KindWeb => "Web", "Web";
    KindApplication => "Application", "Application";
    KindUnknown => "Unknown", "Inconnu";
    AnyTag => "Any tag", "Tous les tags";
    AnyResolution => "Any resolution", "Toutes résolutions";
    ResHd => "Up to 1080p", "Jusqu'à 1080p";
    ResQhd => "1440p", "1440p";
    ResUhd => "4K and more", "4K et plus";
    ResUnknown => "Adaptive or unknown", "Adaptative ou inconnue";
    SortName => "Name", "Nom";
    SortRecent => "Recently added", "Ajout récent";
    SortKind => "Type", "Type";
    ClearFilters => "Clear filters", "Effacer les filtres";
    CountShown => "{shown} of {n} wallpapers", "{shown} sur {n} fonds d'écran";
    NoMatch => "No wallpaper matches", "Aucun fond d'écran ne correspond";
    Scanning => "Looking for wallpapers…", "Recherche des fonds d'écran…";

    NoSteamTitle => "Steam was not found", "Steam est introuvable";
    NoSteamBody => "Lumenwall plays wallpapers from the Wallpaper Engine Workshop on Steam. Install Steam, or add a folder that contains wallpapers.",
        "Lumenwall lit les fonds d'écran du Workshop de Wallpaper Engine sur Steam. Installez Steam, ou ajoutez un dossier qui contient des fonds d'écran.";
    SearchedIn => "Looked in:", "Emplacements vérifiés :";
    NoWallpapersTitle => "No wallpapers yet", "Aucun fond d'écran pour l'instant";
    NoWallpapersBody => "Subscribe to wallpapers in the Wallpaper Engine Workshop on Steam: they appear here once downloaded. You can also add a folder.",
        "Abonnez-vous à des fonds d'écran dans le Workshop de Wallpaper Engine sur Steam : ils apparaissent ici une fois téléchargés. Vous pouvez aussi ajouter un dossier.";
    OpenWorkshop => "Open the Workshop", "Ouvrir le Workshop";
    AddFolder => "Add a folder…", "Ajouter un dossier…";
    NoWeTitle => "Wallpaper Engine is not installed", "Wallpaper Engine n'est pas installé";
    NoWeBody => "Scene wallpapers use files that come with Wallpaper Engine. Install it from Steam to play scenes; videos and web wallpapers work without it.",
        "Les fonds d'écran de type scène utilisent des fichiers fournis avec Wallpaper Engine. Installez-le depuis Steam pour lire les scènes ; les vidéos et les pages web fonctionnent sans.";

    OnScreen => "On screen", "À l'écran";
    NeedsWe => "Needs Wallpaper Engine", "Nécessite Wallpaper Engine";
    Unsupported => "Not supported", "Non pris en charge";
    UnsupportedApp => "Application wallpapers run programs: Lumenwall never runs them.",
        "Les fonds d'écran de type application exécutent des programmes : Lumenwall ne les lance jamais.";
    UnsupportedUnknown => "Lumenwall does not know this wallpaper type.", "Lumenwall ne connaît pas ce type de fond d'écran.";

    Apply => "Apply", "Appliquer";
    AllScreens => "All screens", "Tous les écrans";
    MultiSelectHint => "Click screens to pick where it goes", "Cliquez les écrans à utiliser";
    Scaling => "Scaling", "Mise à l'échelle";
    Fill => "Fill", "Remplir";
    FillHelp => "Cover the screen, cropping the edges", "Couvrir l'écran en rognant les bords";
    FitHelp => "Show everything, with bars if needed", "Tout afficher, avec des bandes si besoin";
    StretchHelp => "Cover the screen, distorting the image", "Couvrir l'écran en déformant l'image";
    Fit => "Fit", "Ajuster";
    Stretch => "Stretch", "Étirer";
    Customize => "Customize", "Personnaliser";
    WallpaperSound => "Wallpaper sound", "Son du fond d'écran";
    WallpaperSoundHint => "Relative to the general volume in the status bar.",
        "Relatif au volume général de la barre d'état.";
    Reset => "Reset", "Réinitialiser";
    NoProperties => "This wallpaper has no settings.", "Ce fond d'écran n'a pas de réglages.";
    LiveHint => "Changes show live on the screens playing this wallpaper.",
        "Les changements s'affichent en direct sur les écrans qui lisent ce fond d'écran.";
    NotAppliedHint => "Your changes are sent when you apply this wallpaper.",
        "Vos changements seront envoyés quand vous appliquerez ce fond d'écran.";
    OpenFolder => "Open folder", "Ouvrir le dossier";
    OpenInSteam => "Open in Steam", "Ouvrir dans Steam";
    NoneValue => "None", "Aucun";
    NeedEngine => "Start the engine to apply wallpapers", "Lancez le moteur pour appliquer des fonds d'écran";
    Applied => "Applied", "Appliqué";
    OriginWorkshop => "Steam Workshop", "Workshop Steam";
    OriginWe => "Wallpaper Engine", "Wallpaper Engine";
    OriginLocal => "Local folder", "Dossier local";
    SchemeColor => "Scheme color", "Couleur du thème";
    Description => "Description", "Description";

    NoScreens => "The engine reports no screen.", "Le moteur ne signale aucun écran.";
    ScreensOffline => "Start the engine to see and set up your screens.",
        "Lancez le moteur pour voir et configurer vos écrans.";
    ChangeWallpaper => "Change", "Changer";
    RemoveWallpaper => "Remove", "Retirer";
    PlaylistPick => "Play a Wallpaper Engine playlist", "Lancer une playlist Wallpaper Engine";
    PlaylistPosition => "Playlist {name} · {n} of {count}", "Playlist {name} · {n} sur {count}";
    PlaylistOutside => "Playlist {name} · {count} wallpapers", "Playlist {name} · {count} fonds";
    PlaylistEvery => "every {n} min", "toutes les {n} min";
    PlaylistManual => "changes with Next only", "change seulement avec Suivant";
    PlaylistRandom => "random order", "ordre aléatoire";
    PlaylistNext => "Next", "Suivant";
    PlaylistPrevious => "Previous", "Précédent";
    PlaylistStop => "Stop the playlist", "Arrêter la playlist";
    PlaylistNone => "No Wallpaper Engine playlist yet: make one in Wallpaper Engine.", "Aucune playlist Wallpaper Engine pour l'instant : créez-en une dans Wallpaper Engine.";
    PlaylistUnavailable => "Playlists unavailable: {e}", "Playlists indisponibles : {e}";
    EmptyScreen => "No wallpaper", "Aucun fond d'écran";
    StatusLoading => "Loading", "Chargement";
    StatusError => "Error", "Erreur";
    Primary => "Primary", "Principal";
    PickForScreen => "Pick a wallpaper for {screen}", "Choisissez un fond d'écran pour {screen}";

    SecPerformance => "Performance", "Performance";
    FpsLimit => "Frame rate limit", "Limite d'images par seconde";
    FpsHelp => "Lower is lighter on CPU, GPU and battery.", "Plus bas, c'est moins de CPU, de GPU et de batterie.";
    PauseFullscreen => "Pause when an app is fullscreen", "Mettre en pause quand une application est en plein écran";
    PauseBattery => "Pause on battery", "Mettre en pause sur batterie";
    PauseLock => "Pause when the session is locked", "Mettre en pause quand la session est verrouillée";
    SecAudio => "Sound", "Son";
    Muted => "Muted", "Son coupé";
    Automute => "Mute while another app plays media", "Couper le son pendant la lecture d'un média";
    AutomuteHint => "Music, videos and games. Calls, notifications and paused players are ignored.",
        "Musique, vidéos et jeux. Les appels, les notifications et les lecteurs en pause sont ignorés.";
    SecStartup => "Startup", "Démarrage";
    Autostart => "Start Lumenwall with my session", "Lancer Lumenwall à l'ouverture de session";
    TrayHint => "The tray icon needs a system tray. On GNOME, install the AppIndicator extension; Lumenwall works without it.",
        "L'icône de notification nécessite une zone de notification. Sous GNOME, installez l'extension AppIndicator ; Lumenwall fonctionne sans.";
    SecLibrary => "Library", "Bibliothèque";
    Folders => "Extra wallpaper folders", "Dossiers de fonds d'écran supplémentaires";
    NoFolders => "None. The Steam Workshop is always included.", "Aucun. Le Workshop Steam est toujours inclus.";
    FolderPath => "Folder path", "Chemin du dossier";
    Add => "Add", "Ajouter";
    Remove => "Remove", "Retirer";
    Rescan => "Rescan", "Actualiser";
    FolderMissing => "This folder does not exist", "Ce dossier n'existe pas";
    Detected => "Detected", "Détecté";
    NotFound => "not found", "introuvable";
    SteamLibraries => "Steam libraries", "Bibliothèques Steam";
    SecAppearance => "Appearance", "Apparence";
    Language => "Language", "Langue";
    LangSystem => "System", "Système";
    Theme => "Theme", "Thème";
    ThemeDark => "Dark", "Sombre";
    ThemeLight => "Light", "Clair";
    PreviewAnim => "Animated previews", "Aperçus animés";
    PreviewVisible => "Visible cards", "Cartes visibles";
    PreviewHover => "On hover", "Au survol";
    PreviewNever => "Never", "Jamais";
    EngineSettingsOffline => "Engine settings can be changed while the engine is running.",
        "Les réglages du moteur se modifient quand le moteur est lancé.";
    SecAbout => "About", "À propos";
    AboutText => "Lumenwall {v}: free software under the MIT or Apache-2.0 license. Not affiliated with Wallpaper Engine or Valve.",
        "Lumenwall {v} : logiciel libre sous licence MIT ou Apache-2.0. Sans lien avec Wallpaper Engine ni Valve.";
    Error => "Error", "Erreur";

    TrayOpen => "Open Lumenwall", "Ouvrir Lumenwall";
    TrayQuit => "Quit Lumenwall", "Quitter Lumenwall";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_string_is_translated_and_placeholders_match() {
        for k in K::ALL {
            let (en, fr) = (k.get(Lang::En), k.get(Lang::Fr));
            assert!(!en.is_empty() && !fr.is_empty(), "{k:?}");
            let holes = |s: &str| {
                let mut v: Vec<String> = s
                    .split('{')
                    .skip(1)
                    .filter_map(|p| p.split_once('}'))
                    .map(|(h, _)| h.to_owned())
                    .collect();
                v.sort();
                v
            };
            assert_eq!(holes(en), holes(fr), "{k:?}");
        }
    }
}
