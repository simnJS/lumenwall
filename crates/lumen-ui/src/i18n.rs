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
    AppName => "Lumen", "Lumen";
    NavLibrary => "Library", "Bibliothèque";
    NavScreens => "Screens", "Écrans";
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
    EngineMissingHelp => "Lumen looks for the “lumen” program next to this app, then in PATH.",
        "Lumen cherche le programme « lumen » à côté de cette application, puis dans le PATH.";

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
    SettingsSubtitle => "Lumen stays light: every option here can only lower its cost.", "Lumen reste léger : chaque option ici ne peut qu'en réduire le coût.";
    ApplyAgain => "Apply again", "Réappliquer";
    PreviewAnimHelp => "On hover is the lightest.", "« Au survol » est le plus léger.";
    Close => "Close", "Fermer";
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
    NoSteamBody => "Lumen plays wallpapers from the Wallpaper Engine Workshop on Steam. Install Steam, or add a folder that contains wallpapers.",
        "Lumen lit les fonds d'écran du Workshop de Wallpaper Engine sur Steam. Installez Steam, ou ajoutez un dossier qui contient des fonds d'écran.";
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
    UnsupportedApp => "Application wallpapers run programs: Lumen never runs them.",
        "Les fonds d'écran de type application exécutent des programmes : Lumen ne les lance jamais.";
    UnsupportedUnknown => "Lumen does not know this wallpaper type.", "Lumen ne connaît pas ce type de fond d'écran.";

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
    Automute => "Mute when another app plays sound", "Couper le son quand une autre application en joue";
    SecStartup => "Startup", "Démarrage";
    Autostart => "Start Lumen with my session", "Lancer Lumen à l'ouverture de session";
    TrayHint => "The tray icon needs a system tray. On GNOME, install the AppIndicator extension; Lumen works without it.",
        "L'icône de notification nécessite une zone de notification. Sous GNOME, installez l'extension AppIndicator ; Lumen fonctionne sans.";
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
    AboutText => "Lumen {v}: free software under the MIT or Apache-2.0 license. Not affiliated with Wallpaper Engine or Valve.",
        "Lumen {v} : logiciel libre sous licence MIT ou Apache-2.0. Sans lien avec Wallpaper Engine ni Valve.";
    Error => "Error", "Erreur";

    TrayOpen => "Open Lumen", "Ouvrir Lumen";
    TrayQuit => "Quit Lumen", "Quitter Lumen";
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
