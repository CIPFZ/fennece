app-title = Fennec
search-hint = Rechercher
col-name = Nom
col-path = Chemin
col-size = Taille
col-modified = Modifié le
status-objects = { $count ->
    [one] { $count } objet
   *[other] { $count } objets
}
status-selected = { $count ->
    [one] { $count } sélectionné
   *[other] { $count } sélectionnés
}
status-elapsed = { $ms } ms
index-starting = Démarrage…
index-building = Indexation de { $volume } : { $done } entrées
index-meta = Lecture des tailles et des dates { $percent }%
index-ready = { $count ->
    [one] { $count } entrée indexée en { $seconds } s
   *[other] { $count } entrées indexées en { $seconds } s
}
index-error = Erreur d'index : { $message }
index-db-rebuild = La base de données de l'index est illisible et va être reconstruite ({ $message }).
index-volume-rebuild = Le journal des modifications de { $volume } a changé ; réindexation de ce volume.
index-volume-added = { $volume } a été ajouté à l'index.
index-watch-error = Surveillance de { $volume } arrêtée : { $message }
index-folder-fallback = Pas d'accès au journal NTFS de { $volume } (exécutez en tant qu'administrateur ou installez le service) ; indexation en tant que dossier.
empty-results = Aucun résultat
empty-indexing = Création de l'index…
action-open = Ouvrir
action-open-path = Ouvrir l'emplacement
action-copy-path = Copier le chemin complet
action-copy-name = Copier le nom
tray-show = Afficher Fennec
tray-exit = Quitter
msg-copied = Copié dans le Presse-papiers
msg-open-failed = Impossible d'ouvrir { $path } : { $error }
diag-unsupported = Pas encore pris en charge : { $name }:
diag-invalid = Valeur non valide pour { $name } : { $value }
diag-pattern = Motif non valide : { $message }
diag-quote = Guillemet non fermé
diag-group = < non fermé
msg-restart-required = Redémarrez Fennec pour modifier les dossiers indexés et les listes de fichiers.
cmd-focus-search = Activer la zone de recherche
cmd-refresh = Actualiser
cmd-open = Ouvrir
cmd-open-path = Ouvrir l'emplacement
cmd-open-with = Ouvrir avec…
cmd-run-as-admin = Exécuter en tant qu'administrateur
cmd-properties = Propriétés
cmd-context-menu = Menu contextuel
cmd-copy = Copier
cmd-cut = Couper
cmd-paste = Coller
cmd-copy-full-path = Copier le chemin complet
cmd-copy-full-path-quoted = Copier le chemin complet (entre guillemets)
cmd-copy-name = Copier le nom
cmd-copy-path = Copier le chemin
cmd-copy-as-tsv = Copier en TSV
cmd-copy-as-csv = Copier en CSV
cmd-copy-as-efu = Copier en liste de fichiers
cmd-delete = Supprimer
cmd-delete-permanently = Supprimer définitivement
cmd-rename = Renommer
cmd-new-folder = Nouveau dossier
cmd-explore-in-fennec = Parcourir dans Fennec
cmd-omit = Masquer les résultats
cmd-set-run-count = Définir le nombre d'exécutions…
cmd-select-all = Sélectionner tout
cmd-invert-selection = Inverser la sélection
cmd-toggle-selection = Basculer la sélection
cmd-move-down = Descendre
cmd-move-up = Monter
cmd-extend-down = Étendre la sélection vers le bas
cmd-extend-up = Étendre la sélection vers le haut
cmd-focus-down = Focus vers le bas
cmd-focus-up = Focus vers le haut
cmd-page-down = Page suivante
cmd-page-up = Page précédente
cmd-first = Premier résultat
cmd-last = Dernier résultat
cmd-match-case = Respecter la casse
cmd-match-whole-word = Mot entier
cmd-match-path = Rechercher dans le chemin
cmd-match-diacritics = Respecter les accents
cmd-enable-regex = Activer les expressions régulières
cmd-advanced-search = Recherche avancée…
cmd-add-bookmark = Ajouter aux signets…
cmd-organize-bookmarks = Organiser les signets…
cmd-organize-filters = Organiser les filtres…
cmd-add-filter = Ajouter un filtre…
cmd-filter-everything = Tout
cmd-options = Options…
cmd-export = Exporter…
cmd-toggle-menu-bar = Barre de menus
cmd-toggle-status-bar = Barre d'état
cmd-toggle-filter-bar = Barre de filtres
cmd-theme-light = Clair
cmd-theme-dark = Sombre
cmd-theme-system = Suivre le système
cmd-zoom-in = Zoom avant
cmd-zoom-out = Zoom arrière
cmd-zoom-reset = Réinitialiser le zoom
cmd-search-history = Historique des recherches
cmd-clear-history = Effacer l'historique des recherches
cmd-install-service = Installer le service Fennec
cmd-uninstall-service = Désinstaller le service Fennec
cmd-save-database = Enregistrer l'index maintenant
cmd-pause-updates = Suspendre les mises à jour de l'index
cmd-help = Syntaxe de recherche
cmd-about = À propos de Fennec
cmd-hide-window = Masquer la fenêtre
cmd-exit = Quitter
cmd-filter-1 = Filtre 1
cmd-filter-2 = Filtre 2
cmd-filter-3 = Filtre 3
cmd-filter-4 = Filtre 4
cmd-filter-5 = Filtre 5
cmd-filter-6 = Filtre 6
cmd-filter-7 = Filtre 7
cmd-filter-8 = Filtre 8
cmd-filter-9 = Filtre 9
col-ext = Extension
col-type = Type
col-dm = Modifié le
col-dc = Date de création
col-da = Date d'accès
col-attributes = Attributs
col-runcount = Nombre d'exécutions
col-dr = Date d'exécution
col-rc = Date de modification récente
col-fullpath = Chemin complet
col-filelist = Nom de la liste de fichiers
col-indexsource = Source d'index
col-volume = Volume
col-depth = Profondeur
col-namelength = Longueur du nom
col-frn = Numéro de référence de fichier
options-page-general = Général
options-desc-general = Langue, instance unique, zone de notification et raccourci global.
options-page-ui = Interface
options-desc-ui = Thème, zoom, hauteur des lignes et barres affichées.
options-page-search = Recherche
options-desc-search = Mode de correspondance par défaut des recherches.
options-page-results = Résultats
options-desc-results = Mise en surbrillance, ordre des dossiers et ouverture.
options-page-view = Affichage
options-desc-view = Colonnes, ordre de tri et formats.
options-page-fonts_colors = Polices et couleurs
options-desc-fonts_colors =  
options-page-history = Historique
options-desc-history = Historique des recherches et des exécutions.
options-page-keyboard = Clavier
options-desc-keyboard =  
options-page-context_menu = Menu contextuel
options-desc-context_menu = L'intégration à l'Explorateur de fichiers est installée avec les entrées du menu contextuel (fennec -install-shell, M4).
options-page-open_commands = Commandes d'ouverture
options-desc-open_commands = Commandes d'ouverture personnalisées : Libellé|programme arguments, %1 = chemin complet.
options-page-index = Index
options-desc-index = Ce qui est indexé et comment la base de données est enregistrée.
options-page-ntfs = NTFS / ReFS
options-desc-ntfs = Volumes NTFS/ReFS et leurs journaux des modifications.
options-page-folders = Dossiers
options-desc-folders = Index de dossiers (FAT, réseau, n'importe quel dossier).
options-page-filelists = Listes de fichiers
options-desc-filelists = Listes de fichiers EFU montées.
options-page-exclude = Exclusions
options-desc-exclude = Fichiers cachés/système et dossiers exclus.
options-page-properties = Propriétés
options-desc-properties = Indexation des propriétés (largeur, durée, auteur…).
options-page-content = Contenu
options-desc-content = Indexation du contenu.
options-page-semantic = Recherche sémantique
options-desc-semantic = Recherche sémantique locale avec EmbeddingGemma-2. Téléchargez le modèle et consultez son état dans Outils → Recherche sémantique…
options-page-index_journal = Journal d'index
options-desc-index_journal = Journal d'index (date de modification récente).
options-page-server = Serveurs
options-desc-server = Le serveur IPC pour fes.exe et le SDK, la fenêtre IPC compatible Everything (es.exe, lanceurs) et le serveur HTTP.
options-page-advanced = Avancé
options-desc-advanced =  
keyctx-search = Zone de recherche
keyctx-results = Liste des résultats
keyctx-global = Global
filter-audio = Audio
filter-compressed = Compressé
filter-document = Document
filter-executable = Exécutable
filter-folder = Dossier
filter-picture = Image
filter-video = Vidéo
filter-everything = Tout
menu-file = Fichier
menu-edit = Édition
menu-view = Affichage
menu-search = Recherche
menu-bookmarks = Signets
menu-tools = Outils
menu-help = Aide
menu-copy-as = Copier en
menu-theme = Thème
menu-zoom = Zoom
menu-columns = Colonnes
menu-reset-columns = Réinitialiser les colonnes
tray-options = Options…
tray-pause = Suspendre/reprendre les mises à jour de l'index
status-paused = mises à jour suspendues
type-folder = Dossier de fichiers
type-file = Fichier
source-folder = Dossier
source-filelist = Liste de fichiers
time-just-now = à l'instant
new-folder-name = Nouveau dossier
dialog-ok = OK
dialog-cancel = Annuler
dialog-close = Fermer
dialog-add = Ajouter…
dialog-edit = Modifier…
dialog-delete = Supprimer
dialog-import = Importer depuis Everything
dialog-create = Créer
dialog-search = Rechercher
dialog-save = Enregistrer
dialog-edit-filter = Filtre
dialog-run-count = Nombre d'exécutions :
dialog-export-help = Exporte les résultats dans un fichier ; l'extension détermine le format : .efu (liste de fichiers), .csv / .tsv / .json (colonnes visibles), .m3u / .m3u8, toute autre extension un chemin par ligne.
field-name = Nom
field-search = Recherche
field-filter = Filtre
field-sort = Tri
field-folder = Dossier
field-macro = Macro
builder-all-words = Tous ces mots
builder-exact-phrase = Cette expression exacte
builder-any-words = N'importe lequel de ces mots
builder-none-words = Aucun de ces mots
builder-match-case = Respecter la casse
builder-whole-word = Mots entiers uniquement
builder-folder = Dans le dossier
builder-subfolders = Inclure les sous-dossiers
builder-extensions = Extensions
builder-kind = Rechercher
builder-kind-all = Fichiers et dossiers
builder-kind-files = Fichiers
builder-kind-folders = Dossiers
builder-size = Taille
builder-date = Date de modification
builder-attributes = Attributs
builder-hidden = Caché
builder-system = Système
builder-readonly = Lecture seule
builder-preview = Recherche :
options-title = Options de Fennec
options-ok = OK
options-cancel = Annuler
options-apply = Appliquer
options-reset = Réinitialiser
options-reset-keys = Restaurer les raccourcis par défaut
options-show-containing = Afficher les paramètres contenant :
options-empty-page = Rien à configurer ici pour l'instant.
options-command = Commande
options-press-key = Appuyez sur une touche…
options-filter-hint = Filtrer les commandes
options-keyboard-help = Cliquez sur un raccourci pour le modifier ; ✕ le supprime.
msg-copied-files = { $count ->
    [one] { $count } élément copié dans le Presse-papiers
   *[other] { $count } éléments copiés dans le Presse-papiers
}
msg-pasted = Collé
msg-op-failed = Échec de l'opération : { $error }
msg-deleted = { $count ->
    [one] { $count } élément supprimé
   *[other] { $count } éléments supprimés
}
msg-renamed = Renommé en { $name }
msg-created = Créé : { $name }
msg-db-saved = Index enregistré
msg-service-requested = Commande de service lancée
msg-history-cleared = Historique des recherches effacé
msg-setting-changed = { $key } = { $value }
msg-unknown-command = Commande inconnue : /{ $command }
msg-bad-bindings = Raccourcis non valides ignorés : { $bindings }
msg-updates-paused = Mises à jour de l'index suspendues
msg-updates-resumed = Mises à jour de l'index reprises
msg-imported = { $count ->
    [one] { $count } élément importé depuis { $file }
   *[other] { $count } éléments importés depuis { $file }
}
msg-import-missing = { $file } introuvable (recherché dans les dossiers de données de Fennec et d'Everything)
msg-exported = { $count ->
    [one] { $count } résultat exporté vers { $path }
   *[other] { $count } résultats exportés vers { $path }
}
about-text =
    Trouvez tout en un clin d'œil.
    Un moteur de recherche de noms de fichiers compatible avec la syntaxe de recherche d'Everything 1.5.
time-minutes-ago = il y a { $n } min
time-hours-ago = { $n ->
    [one] il y a { $n } heure
   *[other] il y a { $n } heures
}
time-days-ago = { $n ->
    [one] il y a { $n } jour
   *[other] il y a { $n } jours
}
type-ext-file = Fichier { $ext }
dialog-new-folder-in = Nouveau dossier dans { $path } :
help-syntax =
    Syntaxe de recherche (compatible Everything)
        espace   ET        |   OU        !   NON       < >   regroupement       "…"   texte littéral
        {"*"}  ?    caractères génériques (nom entier)   \ dans un terme cherche dans le chemin complet   ^abc / abc$ début / fin du nom
    Modificateurs : case: ww: wfn: path: regex: diacritics: nopath: py: (pinyin) …
    Fonctions : ext:jpg;png  size:>10mb  dm:today  dc:2026  attrib:H  parent:C:\Windows  child:*.mp3  dupe:  empty:  depth:2 …
    Macros : audio: zip: doc: exe: pic: video:
    Commandes : tapez /search_delay=100 ou /reindex dans la zone de recherche et appuyez sur Entrée.
dialog-confirm-delete = { $count ->
    [one] Supprimer définitivement cet élément ? Cette action est irréversible.
   *[other] Supprimer définitivement ces { $count } éléments ? Cette action est irréversible.
}
cmd-index-journal = Journal d'index…
journal-create = Créé
journal-delete = Supprimé
journal-rename = Renommé
journal-move = Déplacé
journal-modify = Modifié
journal-filter = Afficher les modifications contenant :
journal-count = { $count ->
    [one] { $count } enregistrement
   *[other] { $count } enregistrements
}
journal-time = Heure
journal-action = Modification
journal-before = Avant
journal-after = Après
cmd-toggle-preview = Aperçu
cmd-view-details = Détails
cmd-view-medium-thumbnails = Miniatures moyennes
cmd-view-large-thumbnails = Grandes miniatures
cmd-view-extra-large-thumbnails = Très grandes miniatures
cmd-next-item = Élément suivant
cmd-previous-item = Élément précédent
preview-nothing = Sélectionnez un résultat pour afficher son aperçu.
preview-loading = Chargement…
preview-binary = Fichier binaire (pas d'aperçu texte)
preview-truncated = Seul le premier 1 MB est affiché.
preview-selected = { $count ->
    [one] { $count } élément sélectionné
   *[other] { $count } éléments sélectionnés
}
preview-total-size = Taille totale : { $size }
preview-contents = Contenu
preview-children = { $files ->
        [one] { $files } fichier
       *[other] { $files } fichiers
    }, { $folders ->
        [one] { $folders } dossier
       *[other] { $folders } dossiers
    }
cmd-new-window = Nouvelle fenêtre
cmd-close-window = Fermer la fenêtre
msg-unknown-filter = Filtre inconnu : { $name }
msg-unknown-bookmark = Signet inconnu : { $name }
msg-unknown-sort = Colonne de tri inconnue : { $name }
shell-search-fennec = Rechercher avec Fennec
shell-explore-in-fennec = Parcourir dans Fennec
cmd-import-settings = Importer les paramètres…
cmd-export-settings = Exporter les paramètres…
cmd-import-everything-ini = Importer Everything.ini…
cmd-export-filters = Exporter les filtres…
cmd-export-bookmarks = Exporter les signets…
cmd-export-search-history = Exporter l'historique des recherches…
menu-import-export = Importer / Exporter
dialog-transfer-path = Fichier :
msg-transfer-done = Terminé : { $path }
msg-ini-imported = { $count ->
    [one] { $count } paramètre importé depuis Everything.ini
   *[other] { $count } paramètres importés depuis Everything.ini
}
index-extracting = Propriétés/contenu : { $done } / { $total }
warn-not-indexed = { $name } : non indexé (activez l'index dans les options ou utilisez notindexed:{ $name }:)
warn-no-file-access = { $name } : doit lire les fichiers, ce qui est impossible pour cette recherche
warn-slow-limit = Recherche lente arrêtée après la lecture de { $count } fichiers ; ajoutez des filtres pour l'affiner.
warn-semantic = Recherche sémantique : { $reason }
warn-system-index-unavailable = si: l'index système Windows Search n'est pas disponible (le service Windows Search n'est pas installé ou ne fonctionne pas)
warn-system-index-failed = si: échec de la requête Windows Search : { $reason }
warn-system-index-limit = si: Windows Search a trouvé plus de { $count } éléments ; seuls les { $count } premiers sont utilisés
value-yes = Oui
value-no = Non
unit-pixels = { $count ->
    [one] { $count } pixel
   *[other] { $count } pixels
}
unit-stars = { $count ->
    [one] { $count } étoile
   *[other] { $count } étoiles
}
menu-property-columns = Colonnes de propriétés
menu-property-filter = Filtrer les propriétés
col-dupegroup = Groupe de doublons
preview-hits = { $count ->
    [one] { $count } correspondance
   *[other] { $count } correspondances
}
prop-width = Largeur
prop-height = Hauteur
prop-dimensions = Dimensions
prop-aspect-ratio = Rapport hauteur/largeur
prop-bit-depth = Profondeur de bits
prop-horizontal-resolution = Résolution horizontale
prop-vertical-resolution = Résolution verticale
prop-orientation = Orientation
prop-date-taken = Prise de vue
prop-camera-maker = Fabricant de l'appareil photo
prop-camera-model = Modèle de l'appareil photo
prop-exposure-time = Temps d'exposition
prop-f-stop = Ouverture
prop-iso-speed = Sensibilité ISO
prop-focal-length = Distance focale
prop-flash = Flash
prop-latitude = Latitude
prop-longitude = Longitude
prop-altitude = Altitude
prop-people = Personnes
prop-tags = Mots clés
prop-rating = Notation
prop-title = Titre
prop-subject = Objet
prop-comment = Commentaires
prop-copyright = Copyright
prop-length = Durée
prop-artist = Artiste
prop-album = Album
prop-album-artist = Artiste de l'album
prop-track = {"#"}
prop-disc = Disque
prop-genre = Genre
prop-year = Année
prop-composer = Compositeur
prop-conductor = Chef d'orchestre
prop-publisher = Éditeur
prop-bit-rate = Débit binaire
prop-sample-rate = Fréquence d'échantillonnage
prop-channels = Canaux
prop-bits-per-sample = Bits par échantillon
prop-is-variable-bit-rate = Débit binaire variable
prop-lyrics = Paroles
prop-mood = Ambiance
prop-bpm = Battements par minute
prop-initial-key = Tonalité
prop-encoded-by = Encodé par
prop-frame-width = Largeur d'image
prop-frame-height = Hauteur d'image
prop-frame-rate = Fréquence d'images
prop-video-bitrate = Débit de données
prop-total-bitrate = Débit total
prop-compression = Compression vidéo
prop-video-codec = Codec vidéo
prop-audio-codec = Codec audio
prop-date-encoded = Date d'encodage
prop-director = Réalisateurs
prop-producer = Producteurs
prop-writer = Scénaristes
prop-stream-count = Flux
prop-four-cc = FourCC
prop-author = Auteurs
prop-last-author = Dernier enregistrement par
prop-category = Catégorie
prop-company = Société
prop-manager = Responsable
prop-page-count = Pages
prop-slide-count = Diapositives
prop-word-count = Nombre de mots
prop-char-count = Nombre de caractères
prop-line-count = Nombre de lignes
prop-paragraph-count = Nombre de paragraphes
prop-revision-number = Numéro de révision
prop-template = Modèle de document
prop-date-printed = Dernière impression
prop-date-last-saved = Dernier enregistrement
prop-content-created = Contenu créé
prop-total-editing-time = Durée totale de modification
prop-security = Sécurité
prop-file-version = Version du fichier
prop-product-version = Version du produit
prop-product-name = Nom du produit
prop-file-description = Description du fichier
prop-original-filename = Nom de fichier d'origine
prop-internal-name = Nom interne
prop-legal-trademarks = Marques déposées
prop-private-build = Build privée
prop-special-build = Build spéciale
prop-language = Langue
prop-machine = Plateforme cible
prop-subsystem = Sous-système
prop-is-dll = DLL
prop-is-signed = Signé
prop-linker-version = Version de l'éditeur de liens
prop-os-version = Version du système d'exploitation
prop-image-version = Version de l'image
prop-program-name = Nom de l'application
prop-computer = Ordinateur
prop-content-status = Statut du contenu
prop-content-type = Type de contenu
prop-date-acquired = Date d'acquisition
prop-date-archived = Date d'archivage
prop-date-completed = Date d'achèvement
prop-date-imported = Date d'importation
prop-due-date = Date d'échéance
prop-start-date = Date de début
prop-end-date = Date de fin
prop-item-date = Date
prop-flag-status = Statut de l'indicateur
prop-flag-color = Couleur de l'indicateur
prop-importance = Importance
prop-priority = Priorité
prop-sensitivity = Sensibilité
prop-is-attachment = Est une pièce jointe
prop-is-flagged = Avec indicateur
prop-is-flagged-complete = Indicateur terminé
prop-is-incomplete = Incomplet
prop-is-read = Lu
prop-is-shared = Partagé
prop-shared-with = Partagé avec
prop-sharing-status = État du partage
prop-item-type = Type d'élément
prop-kind-text = Sorte
prop-perceived-type = Type perçu
prop-mime-type = Type MIME
prop-status = Statut
prop-project = Projet
prop-mileage = Kilométrage
prop-participants = Participants
prop-owner = Propriétaire
prop-offline-availability = Disponibilité
prop-offline-status = Statut hors connexion
prop-parental-rating = Contrôle parental
prop-parental-rating-reason = Raison du contrôle parental
prop-software-used = Logiciel utilisé
prop-date-last-used = Dernière utilisation
prop-is-protected = Protégé
prop-play-count = Nombre de lectures
prop-full-name = Nom complet
prop-first-name = Prénom
prop-middle-name = Deuxième prénom
prop-last-name = Nom
prop-nickname = Surnom
prop-job-title = Fonction
prop-department = Service
prop-office-location = Bureau
prop-profession = Profession
prop-gender = Sexe
prop-birthday = Anniversaire
prop-email = Adresse de messagerie
prop-email-addresses = Adresses de messagerie
prop-im-address = Adresse de messagerie instantanée
prop-web-page = Page Web
prop-primary-phone = Téléphone principal
prop-business-phone = Téléphone (bureau)
prop-home-phone = Téléphone (domicile)
prop-mobile-phone = Téléphone mobile
prop-business-address = Adresse (bureau)
prop-business-street = Rue (bureau)
prop-business-city = Ville (bureau)
prop-business-state = État/Province (bureau)
prop-business-postal-code = Code postal (bureau)
prop-business-country = Pays/Région (bureau)
prop-home-address = Adresse (domicile)
prop-home-street = Rue (domicile)
prop-home-city = Ville (domicile)
prop-home-state = État/Province (domicile)
prop-home-postal-code = Code postal (domicile)
prop-home-country = Pays/Région (domicile)
prop-from = De
prop-from-address = Adresse (De)
prop-to = À
prop-to-address = Adresse (À)
prop-cc = Cc
prop-cc-address = Adresse (Cc)
prop-bcc = Cci
prop-bcc-address = Adresse (Cci)
prop-sender = Nom de l'expéditeur
prop-sender-address = Adresse de l'expéditeur
prop-date-received = Date de réception
prop-date-sent = Date d'envoi
prop-has-attachments = Pièces jointes
prop-attachment-names = Noms des pièces jointes
prop-message-class = Classe de message
prop-is-fwd-or-reply = Transféré ou répondu
prop-to-do-title = Titre de la tâche
prop-location = Emplacement
prop-organizer = Organisateur
prop-required-attendees = Participants obligatoires
prop-optional-attendees = Participants facultatifs
prop-resources = Ressources
prop-is-recurring = Périodique
prop-is-online-meeting = Réunion en ligne
prop-reminder-time = Heure du rappel
prop-show-time-as = Afficher la disponibilité
prop-response-status = État de la réponse
prop-billing-information = Informations de facturation
prop-completion-status = État d'achèvement
prop-task-owner = Propriétaire de la tâche
prop-subtitle = Sous-titre
prop-original-release-year = Année de sortie originale
prop-date-released = Date de sortie
prop-encoding-settings = Paramètres d'encodage
prop-creator-application = Application de création
prop-creator-application-version = Version de l'application de création
prop-content-distributor = Distributeur de contenu
prop-provider-rating = Évaluation du fournisseur
prop-protection-type = Type de protection
prop-episode-number = Numéro d'épisode
prop-season-number = Numéro de saison
prop-series-name = Nom de la série
prop-frame-count = Nombre d'images
prop-part-of-set = Partie du coffret
prop-content-group-description = Description du groupe de contenu
prop-period = Époque
prop-display-artist = Artiste affiché
prop-album-id = ID de l'album
prop-is-compilation = Compilation
prop-artist-sort-order = Tri par artiste
prop-album-artist-sort-order = Tri par artiste de l'album
prop-composer-sort-order = Tri par compositeur
prop-aperture = Valeur d'ouverture
prop-max-aperture = Ouverture maximale
prop-shutter-speed = Vitesse d'obturation
prop-brightness = Luminosité
prop-exposure-bias = Correction d'exposition
prop-exposure-index = Indice d'exposition
prop-exposure-program = Programme d'exposition
prop-metering-mode = Mode de mesure
prop-white-balance = Balance des blancs
prop-light-source = Source de lumière
prop-contrast = Contraste
prop-saturation = Saturation
prop-sharpness = Netteté
prop-gain-control = Contrôle du gain
prop-digital-zoom = Zoom numérique
prop-focal-length-35mm = Focale 35 mm
prop-subject-distance = Distance du sujet
prop-lens-maker = Fabricant de l'objectif
prop-lens-model = Modèle de l'objectif
prop-camera-serial-number = Numéro de série de l'appareil photo
prop-flash-maker = Fabricant du flash
prop-flash-model = Modèle de flash
prop-flash-energy = Énergie du flash
prop-exif-version = Version EXIF
prop-event = Événement
prop-focal-plane-x-resolution = Résolution X du plan focal
prop-focal-plane-y-resolution = Résolution Y du plan focal
prop-photometric-interpretation = Interprétation photométrique
prop-program-mode = Mode programme
prop-related-sound-file = Fichier son associé
prop-gps-date = Date GPS
prop-gps-img-direction = Direction de l'image
prop-gps-img-direction-ref = Référence de direction de l'image
prop-gps-speed = Vitesse GPS
prop-gps-speed-ref = Unité de vitesse GPS
prop-gps-track = Cap GPS
prop-gps-track-ref = Référence du cap GPS
prop-gps-dop = DOP GPS
prop-gps-satellites = Satellites GPS
prop-gps-status = État GPS
prop-gps-measure-mode = Mode de mesure GPS
prop-gps-map-datum = Système géodésique GPS
prop-gps-processing-method = Méthode de traitement GPS
prop-gps-area-information = Informations de zone GPS
prop-gps-dest-latitude = Latitude de destination
prop-gps-dest-longitude = Longitude de destination
prop-gps-dest-bearing = Relèvement de destination
prop-gps-dest-distance = Distance de destination
prop-gps-differential = Correction différentielle GPS
prop-color-space = Représentation des couleurs
prop-compressed-bits-per-pixel = Bits compressés/pixel
prop-image-compression = Compression de l'image
prop-image-id = ID de l'image
prop-resolution-unit = Unité de résolution
prop-horizontal-aspect-ratio = Rapport hauteur/largeur horizontal
prop-vertical-aspect-ratio = Rapport hauteur/largeur vertical
prop-video-orientation = Orientation de la vidéo
prop-video-sample-size = Taille d'échantillon vidéo
prop-video-stream-name = Nom du flux vidéo
prop-video-stream-number = Numéro du flux vidéo
prop-is-stereo = Stéréo
prop-is-spherical = Sphérique
prop-audio-format = Format audio
prop-audio-compression = Compression audio
prop-peak-value = Valeur de crête
prop-audio-stream-name = Nom du flux audio
prop-audio-stream-number = Numéro du flux audio
prop-contributors = Collaborateurs
prop-division = Division
prop-document-id = ID du document
prop-client-id = ID client
prop-document-version = Numéro de version
prop-byte-count = Nombre d'octets
prop-hidden-slide-count = Diapositives masquées
prop-note-count = Notes
prop-multimedia-clip-count = Clips multimédias
prop-presentation-format = Format de présentation
prop-url = URL
prop-link-arguments = Arguments
prop-link-comment = Commentaire du lien
prop-link-description = Description du lien
prop-link-status = État du lien
prop-link-target-extension = Extension de la cible du lien
prop-date-visited = Date de visite
sem-err-no-model = La recherche sémantique nécessite le fichier de modèle { $path } (Options → Index → Recherche sémantique → Télécharger le modèle)
sem-err-no-images = La recherche d'images (semimage:) nécessite le projecteur multimodal, absent de cette version
sem-err-no-url = Ce modèle n'a pas d'adresse de téléchargement ; téléchargez-le vous-même et indiquez son fichier dans les options
sem-status-idle = Sémantique : aucun dossier
sem-status-no-model = Sémantique : modèle manquant
sem-status-loading = Sémantique : chargement du modèle
sem-status-ready = Sémantique : prête ({ $chunks } segments)
sem-status-indexing = Sémantique : indexation de { $chunks } segments ({ $percent }%)
sem-status-paused = Sémantique : en pause
sem-status-error = Sémantique : { $error }
sem-status-download = Sémantique : téléchargement du modèle ({ $percent }%)
sem-download-cancelled = Téléchargement annulé
sem-download-bad-size = Le fichier téléchargé fait { $got } octets au lieu de { $want }
sem-download-bad-hash = Le fichier téléchargé ne correspond pas à sa somme de contrôle SHA-256
cmd-semantic-setup = Recherche sémantique…
sem-toggle = Sémantique
sem-toggle-tip = Rechercher aussi par le sens (ajoute ?sem: à chaque recherche ; les résultats sont fusionnés)
sem-download-done = Le modèle sémantique a été téléchargé et vérifié
sem-download-failed = Échec du téléchargement du modèle : { $error }
sem-setup-title = Recherche sémantique
sem-setup-intro = La recherche sémantique trouve les fichiers par leur sens (« tableau budgétaire du dernier trimestre », « comment renouveler une clé d'API ») en plus de la recherche littérale habituelle. Elle s'exécute entièrement sur cet ordinateur : le modèle EmbeddingGemma-2 (environ 300 MB) n'est chargé qu'en cas de besoin, et les vecteurs occupent environ 0,5–1 KB par segment. Seuls les dossiers ci-dessous sont vectorisés, en arrière-plan et avec une limite d'utilisation du CPU.
sem-setup-unavailable = Cette version de Fennec n'inclut pas la recherche sémantique.
sem-setup-folders = Dossiers :
sem-setup-folders-hint = Séparez les dossiers par ; — laissez vide pour utiliser vos dossiers Documents et Bureau.
sem-setup-model = Modèle : { $name }
sem-setup-model-ok = Le fichier du modèle est présent.
sem-setup-download = Télécharger le modèle ({ $mb } MB)
sem-setup-stats = { $docs } fichiers, { $chunks } segments, { $size } en mémoire
sem-setup-enable-first = Activez la recherche sémantique pour choisir le modèle.
sem-setup-enable = Activer la recherche sémantique
sem-setup-apply = Appliquer
sem-setup-rebuild = Reconstruire l'index sémantique
sem-setup-loaded = Le modèle est en mémoire.
sem-setup-unloaded = Le modèle n'est pas en mémoire (il sera chargé à la prochaine recherche sémantique).
status-http = HTTP : { $url }
status-http-failed = Serveur HTTP : { $error }
msg-http-bad-bind = Adresse d'écoute HTTP non valide : { $value }
msg-everything-running = Everything est en cours d'exécution : la fenêtre IPC compatible Everything n'a pas été créée.
msg-http-no-password = un mot de passe est requis (Options → Serveurs)
msg-http-bind-failed = impossible d'écouter sur { $addr } : { $error }
msg-ipc-failed = Serveur IPC non démarré : { $error }
http-search = Rechercher
http-results = { $count ->
    [one] { $count } résultat
   *[other] { $count } résultats
}
http-previous = Précédent
http-next = Suivant
cmd-advanced-rename = Renommage avancé…
cmd-undo = Annuler
cmd-redo = Rétablir
cmd-undo-action = Annuler { $action }
cmd-redo-action = Rétablir { $action }
undo-delete = la suppression
undo-rename = le renommage
undo-new-folder = la création du dossier
undo-advanced-rename = le renommage avancé
msg-undone = Annulation effectuée : { $action }
msg-redone = Rétablissement effectué : { $action }
msg-nothing-to-undo = Rien à annuler
msg-nothing-to-redo = Rien à rétablir
msg-renamed-many = { $count ->
    [one] { $count } élément renommé
   *[other] { $count } éléments renommés
}
rename-title = { $count ->
    [one] Renommage avancé ({ $count } élément)
   *[other] Renommage avancé ({ $count } éléments)
}
rename-template = Nouveau nom :
rename-template-help = $name: $stem: $ext: $parent: $size: $dm: $dc: et $n (compteur)
rename-find = Rechercher :
rename-replace = Remplacer par :
rename-regex = Expression régulière
rename-match-case = Respecter la casse
rename-keep-ext = Conserver l'extension
rename-trim = Supprimer les espaces superflus
rename-case = Casse :
rename-case-keep = Inchangée
rename-case-lower = minuscules
rename-case-upper = MAJUSCULES
rename-case-title = Majuscule À Chaque Mot
rename-case-sentence = Majuscule en début de phrase
rename-counter = Compteur ($n) :
rename-start = début
rename-step = pas
rename-digits = chiffres
rename-old = Nom actuel
rename-new = Nouveau nom
rename-apply = { $count ->
    [one] Renommer { $count } élément
   *[other] Renommer { $count } éléments
}
rename-problems = { $count ->
    [one] { $count } nom inutilisable
   *[other] { $count } noms inutilisables
}
rename-problem-empty = nom vide
rename-problem-invalid = caractères non valides
rename-problem-duplicate = nom en double
rename-problem-exists = existe déjà
rename-problem-pattern = expression régulière non valide
tab-new = Nouvel onglet
cmd-new-tab = Nouvel onglet
cmd-close-tab = Fermer l'onglet
cmd-next-tab = Onglet suivant
cmd-previous-tab = Onglet précédent
cmd-find-in-results = Rechercher dans les résultats…
cmd-find-next = Rechercher le suivant
cmd-find-previous = Rechercher le précédent
find-label = Rechercher :
find-case = Respecter la casse
find-whole-word = Mot entier
find-regex = Expression régulière
find-next = Suivant (F3)
find-previous = Précédent (Shift+F3)
find-none = Introuvable
find-count = { $count ->
    [one] { $count } correspondance
   *[other] { $count } correspondances
}
find-close = Fermer (Esc)
cmd-navigate-back = Précédent
cmd-navigate-forward = Suivant
cmd-toggle-sidebar = Barre latérale
cmd-toggle-toolbar = Barre d'outils
sidebar-folders = Dossiers
sidebar-bookmarks = Signets
sidebar-no-bookmarks = Aucun signet pour l'instant (Signets → Ajouter aux signets).
cmd-theme-high-contrast = Contraste élevé
col-relevance = Pertinence
col-linktarget = Cible du lien
col-filesignature = Signature de fichier
col-recyclebinoriginalpath = Emplacement d’origine (Corbeille)
col-custom = Personnalisée { $n }
cmd-rebuild-index = Reconstruire l’index
msg-rebuilding-index = Reconstruction de l’index ; l’index actuel reste utilisé jusqu’à la fin
options-password-set = (défini — saisir pour modifier)
undo-copy = la copie
undo-move = le déplacement

## ETP/FTP server and client (ADR-036)
cmd-connect-etp = Se connecter à un serveur ETP…
cmd-disconnect-etp = Se déconnecter du serveur ETP
options-page-etp = Serveur ETP/FTP
options-desc-etp = Permet aux clients Everything et FTP de rechercher et de parcourir cet index (ETP = FTP avec l’extension Everything), et réglages de Outils → Se connecter à un serveur ETP. Le nom d’utilisateur et le mot de passe sont envoyés en clair.
status-etp = ETP/FTP : { $url }
status-etp-failed = Serveur ETP/FTP : { $error }
status-etp-remote = ETP : { $host } ({ $total } résultats)
msg-etp-bad-bind = Adresse d’écoute ETP/FTP non valide : { $value }
msg-etp-no-password = un mot de passe est requis (Options → Serveur ETP/FTP)
msg-etp-bad-port = Numéro de port non valide
msg-etp-bad-host = Serveur non valide : utilisez hôte, hôte:port ou utilisateur:mot de passe@hôte:port
msg-etp-connecting = Connexion à { $host }…
msg-etp-connected = Connecté au serveur ETP { $host }
msg-etp-disconnected = Connexion ETP fermée : { $error }
msg-etp-query-failed = Échec de la recherche ETP : { $error }
msg-etp-downloading = { $count ->
    [one] Téléchargement d’un fichier…
   *[other] Téléchargement de { $count } fichiers…
}
msg-etp-download-failed = Échec du téléchargement : { $error }
msg-etp-not-available = Non disponible pour les résultats d’un serveur ETP
etp-host = Hôte :
etp-port = Port :
etp-username = Nom d’utilisateur :
etp-password = Mot de passe :
etp-link-type = Type de lien :
etp-link-ftp = ftp://hôte/C: (téléchargement via le serveur)
etp-link-server = C: (chemins du serveur)
etp-link-share = \\Serveur\C (partages Windows)
etp-link-admin-share = \\Serveur\C$ (partages administratifs)
etp-plain-text-warning = Le nom d’utilisateur et le mot de passe sont envoyés en clair.
etp-connect = Se connecter

# Sorting by the file-reading columns (REL-06)
dialog-read-files-title = Trier selon le contenu des fichiers
dialog-read-files = { $count ->
    [one] 1 fichier doit être lu (environ { $secs } s). Continuer ?
   *[other] { $count } fichiers doivent être lus (environ { $secs } s). Continuer ?
}
dialog-continue = Continuer
status-reading-files = Lecture { $done } / { $total }
