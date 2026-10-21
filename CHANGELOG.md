# Journal des versions

Toutes les versions de Luminux IA, de la plus récente à la plus ancienne. Chaque release GitHub
reprend la section de sa version.

## [0.28.3] - 2026-10-21

- Générer : la teinte du fond de la zone générée est raccordée à celle de la photo autour (fini le carré
  plus clair visible avec le moteur local).
- Gomme IA et Générer : bouton « Ajouter / Retirer » pour enlever une partie de la sélection sans Alt
  (le bureau prend souvent Alt + glisser pour déplacer la fenêtre).
- Copier, couper et coller (Ctrl+C, Ctrl+X, Ctrl+V) fonctionnent dans les zones de texte.

## [0.28.2] - 2026-10-21

- Dépôt public Cfrancky77/Luminux-IA-public : code source et AppImage de chaque version, sans l'historique
  de développement. Les liens de l'aide (mode d'emploi, journal des versions, code source) y mènent.

## [0.28.1] - 2026-10-21

- Fenêtre de vignettes : les aperçus arrivent tout de suite, même dans un dossier de centaines de photos.
  Seules les vignettes à l'écran sont calculées, et un JPEG d'appareil donne sa vignette intégrée (EXIF)
  au lieu d'être décodé en entier ; jusqu'à 5000 images par dossier au lieu de 400.
- Navigation sans le sélecteur du système (qui n'affiche pas d'aperçus) : raccourcis Dossier personnel,
  Images, Bureau, Téléchargements, dossiers de la bibliothèque, disques et clés montés ; chemin cliquable
  élément par élément, ou à taper (Entrée). « Autre dossier… » est retiré.

## [0.28.0] - 2026-10-21

- Gomme IA bien plus rapide sur les grandes photos : 51 s ramenées à 8 s pour retirer des branches d'une
  photo de 18 Mpx (mesure sur 4 cœurs). Le remplissage avance par morceaux entiers au lieu de pixel par
  pixel, la recherche de morceaux se répartit sur les cœurs, et la zone de travail autour de longs traits
  fins est limitée.
- Durée des traitements : la gomme, la génération, l'agrandissement et la fusion affichent un compteur
  pendant le calcul, puis leur durée dans la barre d'état ; dans l'Historique, la ligne de la retouche
  la reprend (« Retouche · 28 s »).

## [0.27.1] - 2026-10-21

- « Ajouter un dossier… » passe aussi par la fenêtre de vignettes : le sélecteur de dossiers du système
  n'affiche pas d'aperçus. Bouton « Ajouter « dossier » » pour le dossier affiché.

## [0.27.0] - 2026-10-21

- Ouvrir une photo (Ctrl+O) et choisir le logo du filigrane passent par la fenêtre de vignettes, comme les
  calques : le sélecteur du système n'affichait que des icônes. Il reste accessible par « Sélecteur du système… ».
  Vignettes décodées sur quatre fils, les premières arrivent plus vite.
- Calques : rotation, au curseur « Rotation » ou à la poignée ronde au-dessus du calque (Maj : pas de 15°).
- Calques : déplacement plus fluide, le calque suit exactement le pointeur et l'histogramme attend le lâcher.

## [0.26.0] - 2026-10-21

- Calques : mode « Gommer » au pinceau sur le calque (Alt pour rendre), « Retirer la gomme ».
- Calques : « Sujet seul (détourage IA) » ne garde que le sujet de l'image posée.
- Calques : « Harmoniser » accorde teinte, noirs et blancs du calque à ceux de la photo.
- Calques : « Fusionner dans une nouvelle image » crée un PNG 16 bits avec les calques, ouvert aussitôt.
- Textes d'aide plus grands (12 px au lieu de 9).
- Moteur local : Débruitage IA grisé (il demande ComfyUI), options Ollama repliées et facultatives ;
  une traduction impossible ne bloque plus la génération.
- Choix de l'image d'un calque : sous-dossiers affichés en texte (l'icône s'affichait en carré).

## [0.25.0] - 2026-10-21

- Moteur IA local, sans serveur : dans Préférences, « Moteur de génération IA » choisit ComfyUI (serveur) ou
  Local (ce PC). Le local est stable-diffusion.cpp (Vulkan, processeur en secours), téléchargé par l'appli
  avec ses modèles conseillés : SD 1.5 inpainting (2,1 Go) et RealESRGAN x4 (64 Mo). Il sert à Générer,
  Agrandir le cadre, Agrandir l'image et à la génération d'un nouveau document. Autre modèle au choix.

## [0.24.0] - 2026-10-21

- Calques : choix de l'image dans une fenêtre de vignettes au lieu d'une liste de noms de fichiers.
- Calques : déplacement à la souris directement sur la photo, cadre du calque affiché.
- Calques : découpe au lasso, seule la partie entourée de l'image est gardée.

## [0.23.1] - 2026-10-21

- Pinceau plus fin pour les petits détails : diamètre minimal 0,1 % de la largeur au lieu de 0,6 %
  (gomme, génération, masques ; 0,2 % pour le correcteur). Sous le curseur « Taille », la taille
  réelle en pixels de la photo est affichée. Touches [ et ] pour réduire ou agrandir.

## [0.23.0] - 2026-10-21

- Gomme IA plus nette : après LaMa, la zone est recomposée avec des morceaux de la photo prélevés autour
  (vagues, herbe, grain), au lieu de la texture lissée par le passage en 512x512. Sans le modèle LaMa,
  ces morceaux remplacent le remplissage flou d'avant.
- Panneau des réglages masqué : une fine bande reste à sa place avec le bouton pour le réafficher,
  au lieu d'un bouton à l'autre bout de la fenêtre.

## [0.22.1] - 2026-10-21

- Bouton à double chevron pour masquer le panneau des réglages : en haut du panneau, à côté de son titre,
  et en haut de la barre d'icônes, où il reste visible pour le réafficher. Le menu Affichage dit désormais
  « Masquer le panneau Réglages ».

## [0.22.0] - 2026-10-21

- Petites animations : les sections des réglages s'ouvrent et se ferment en glissant, le panneau masqué
  par P glisse hors de la fenêtre, et « Avant » (touche B) passe en fondu entre le résultat et l'original.
- Menu Accessibilité : « Sans animations » pour tout afficher d'un coup.

## [0.21.0] - 2026-10-21

- Bibliothèque : « Mettre à la corbeille » (touche Suppr ou clic droit), avec confirmation, pour une photo ou
  toute la sélection. Fichier et réglages vont dans la corbeille du système, restaurables depuis le
  gestionnaire de fichiers ; les copies virtuelles de la photo sont supprimées.
- Panneau des réglages masquable : touche P, menu Affichage, ou re-clic sur l'icône de l'outil ouvert.
  La barre d'icônes reste visible pour le rouvrir.

## [0.20.0] - 2026-10-21

- Thème anthracite : gris légèrement bleuté au lieu du gris neutre, coins arrondis, bleu d'accent plus vif.
  Le mode « Contraste élevé » ne change pas.
- Curseurs de couleur à piste colorée : température, teinte, saturation, virage partiel, balance des couleurs, TSL.
- Réglage modifié signalé par un point orange, sur le curseur et sur le titre de sa section ; la flèche
  circulaire du titre remet la section à zéro.
- Ouverture d'une photo : sa vignette s'affiche, assombrie, le temps du décodage.

## [0.19.1] - 2026-10-21

- Historique : section repliable du bloc Réglages, sous TSL, au lieu d'un onglet à part. H l'ouvre ou la ferme.

## [0.19.0] - 2026-10-21

- Calques : jusqu'à trois images posées sur la photo (texture, logo, autre photo), avec opacité, mode de
  fusion, zone (partout, ciel, sujet, fond), taille et position. Gardés à l'export.

## [0.18.0] - 2026-10-21

- Retouche : sélection au lasso ou au rectangle, en plus du pinceau, pour la gomme et la génération.
- Gomme : choix du remplissage, « IA (LaMa) » ou « Fond uni (rapide) » sans IA.

## [0.17.0] - 2026-10-21

- Bibliothèque : filtres « Floues » (photos probablement floues) et « Semblables » (doublons et rafales,
  la plus nette en tête). Mesure faite une fois par photo, en arrière-plan.
- Colonne Informations : netteté mesurée de la photo.

## [0.16.0] - 2026-10-21

- Bibliothèque : champ de recherche (Ctrl+F) par année, mois, date, appareil, mot-clé, nom ou dossier.
- Traduction par Ollama : en « Auto », les modèles de vision (moondream, llava, qwen-vl…) ne sont plus choisis.

## [0.15.0] - 2026-10-21

- Export : auteur et copyright écrits dans les EXIF de chaque photo exportée.
- Export : filigrane texte ou logo, placé dans un coin ou au centre, avec sa largeur et son opacité.
- Les fenêtres défilantes prennent toute la hauteur disponible au lieu d'une petite zone.

## [0.14.0] - 2026-10-20

- Export : nouveaux formats WebP et AVIF, à côté de JPEG, PNG et TIFF. AVIF reste en sRGB.
- Export d'une seule photo : le format se choisit dans la fenêtre Exporter (avant, seule l'extension du
  fichier le décidait).
- Historique : onglet du panneau de droite au lieu d'une fenêtre flottante.
- Barre sous les menus allégée : plus de doublons des menus, seulement la vue (tri, filtre, avant/après,
  zoom) et la note.
- Les fenêtres (Exporter, Préférences, Raccourcis…) et la barre d'onglets défilent quand l'interface est
  agrandie.

## [0.13.0] - 2026-10-20

- Barre de menus : Fichier, Édition, Affichage, Bibliothèque, Accessibilité, Aide, avec les raccourcis écrits.
  « Nouveau », « Ouvrir » et « Préférences » quittent la barre d'outils pour les menus.
- Menu Accessibilité : taille de l'interface (100 à 200 %), contraste élevé, grandes zones de clic.
- Lecteur d'écran (Orca) : l'interface est exposée par AccessKit, les vignettes sont annoncées par leur nom.
- Échap ferme la fenêtre ouverte (Préférences, Historique, Export, Raccourcis…). F1 : liste des raccourcis.
- Bibliothèque : infobulle sur les vignettes et colonne « Informations » (dossier, poids, dimensions,
  appareil, objectif, prise de vue), touche I.
- « À propos » passe dans le menu Aide.

## [0.12.0] - 2026-10-20

- Bibliothèque : nombre de photos à côté de chaque dossier et total dans « Toutes les photos ».
- Bibliothèque : sous-dossiers dépliables ; un clic n'affiche que les photos du sous-dossier.
- Les dossiers sont relus au retour dans l'application et toutes les 15 minutes (photos ajoutées,
  modifiées ou supprimées), en plus du bouton « Actualiser ».
- Un dossier absent (disque ou partage réseau non monté) garde ses photos dans la bibliothèque.
- Préférences : section « À propos » (version, carte graphique, données, copie des informations).
- Ce journal des versions.

## [0.11.1] - 2026-10-20

- Masques IA en arrière-plan : le compteur affichait le total avant le nombre fait.
- Masques en arrière-plan limités au dossier affiché (500 photos au plus).
- Masques en cache compressés (plusieurs fois moins de place).

## [0.11.0] - 2026-10-20

- Corps IA : chaque personne d'une photo de groupe est traitée à part (quatre au plus).
- Nouveau curseur « Ventre ».
- Corrigé : couleurs fausses dans les PNG exportés (depuis la 0.8.0).
- Corrigé : profils ICC intégrés jugés invalides par certains logiciels.

## [0.10.0] - 2026-10-20

- Disposition « Réglages à part » : curseurs et courbes à l'opposé de la barre d'outils.

## [0.9.0] - 2026-10-20

- Préchargement de la photo suivante.
- Masques Sujet et Ciel calculés en arrière-plan.

## [0.8.0] - 2026-10-20

- Fenêtre Exporter (Ctrl+E) : sRGB, Display P3 ou Adobe RGB avec profil ICC, EXIF gardés (GPS
  en option), taille par bord long, netteté écran ou impression.

## [0.7.0] - 2026-10-20

- Copies virtuelles, mots-clés, ouverture de l'emplacement du fichier.

## [0.6.0] - 2026-10-20

- Historique cliquable (touche H) et versions nommées d'une photo.

## [0.5.4] - 2026-10-20

- Panneau Objectif stable, fenêtre Préférences (langue, barre d'outils à gauche ou à droite).

## [0.5.3] - 2026-10-20

- La Gomme IA ne plante plus en silence.

## [0.5.2] - 2026-10-20

- Taille du correcteur, source prise dans l'image, gomme sur le processeur.

## [0.5.1] - 2026-10-20

- Ciel et zones gommées, correcteur plus clair, profil de l'écran en boutons.

## [0.5.0] - 2026-10-20

- IA sur la carte graphique, profil ICC de l'écran.
- Visage aminci, yeux agrandis, Corps IA.

## [0.4.0] - 2026-10-20

- Interface en anglais, avant/après à rideau, ombres et hautes lumières locales.

## [0.3.1] - 2026-10-20

- Outils ComfyUI signalés, aide et liens des modèles.

## [0.3.0] - 2026-10-20

- Débruitage IA (SCUNet via ComfyUI).

## [0.2.0] - 2026-10-20

- Gomme arrondie, agrandissement par tuiles, README illustré.

## [0.1.0] - 2026-10-20

- Première version : éditeur non destructif (RAW, réglages, courbes, masques), bibliothèque,
  outils IA (ciel, sujet, profondeur, visage, peau), GenExpand et Upscale via ComfyUI,
  détourage, AppImage construite par GitHub Actions.
