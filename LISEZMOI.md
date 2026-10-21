# Luminux IA — mode d'emploi

Éditeur photo non destructif, rendu GPU (wgpu / Vulkan).

## Compiler et lancer

    cargo build --release
    ./target/release/luminux                     # fenêtre vide, Ctrl+O pour ouvrir
    ./target/release/luminux ~/Desktop/Luminux_IA/IMG_1825.CR2

Export sans fenêtre (utilise les réglages du fichier .luminux) :

    ./target/release/luminux --export photo.CR2 sortie.jpg
    ./target/release/luminux --detourer photo.CR2 sortie.png   # fond transparent

## Bibliothèque

- « Ajouter un dossier… » : le dossier et ses sous-dossiers sont parcourus (RAW, JPEG, PNG, TIFF, WebP).
  Au démarrage, les dossiers sont re-parcourus : nouvelles photos ajoutées, photos supprimées retirées.
- Colonne Dossiers : chaque dossier affiche son nombre de photos ; la flèche déplie ses sous-dossiers, et un clic sur un sous-dossier n'affiche que ses photos (sous-dossiers compris). « Toutes les photos » donne le total de la bibliothèque.
- « Actualiser » (menu Bibliothèque) relit les dossiers (photos ajoutées, modifiées ou supprimées). C'est aussi fait tout seul au retour dans l'application (après au moins une minute) et toutes les 15 minutes. Un dossier absent (disque ou partage réseau non monté) garde ses photos dans la bibliothèque au lieu de les perdre.
- Clic droit sur un dossier : le retirer de la bibliothèque (les fichiers ne sont pas touchés).
- Mettre à la corbeille : touche Suppr ou clic droit sur une vignette, après confirmation. Avec une sélection
  multiple, toutes les photos choisies y vont. Le fichier et ses réglages partent dans la corbeille du système
  (restaurables depuis le gestionnaire de fichiers) ; ses copies virtuelles sont supprimées.
- Survol d'une vignette : nom, date, appareil, taille et dossier. Colonne « Informations » (touche I ou menu
  Bibliothèque) : nom, dossier, poids du fichier, dimensions, date, appareil, objectif, réglages de prise de vue,
  et « Afficher dans le dossier ». Pour un RAW, les dimensions
  n'apparaissent qu'une fois la photo ouverte dans l'éditeur.
- Recherche (champ en haut de la bibliothèque, Ctrl+F) : tous les mots doivent correspondre. Une année
  (« 2024 »), un mois (« juillet » ou « July »), une date (« 14/07/2024 », « 07/2024 »), ou un morceau de
  l'appareil, d'un mot-clé, du nom ou du dossier. Majuscules et accents ne comptent pas (« ete » trouve
  « Été »). Échap ou « × » efface la recherche.
- Tri : plus récentes, plus anciennes, nom.
- Tri des photos : « Floues » montre les photos probablement floues, les moins nettes d'abord ; « Semblables »
  regroupe doublons et rafales, la plus nette de chaque groupe en premier. La netteté et l'empreinte sont
  mesurées en arrière-plan une fois par photo (sur un aperçu de 1024 px), puis gardées dans le catalogue ; une
  photo modifiée sur le disque est remesurée. La netteté se lit aussi dans la colonne Informations. La mesure
  prend les zones les plus nettes : un portrait au fond flou volontaire reste net si le sujet l'est. C'est une
  aide au tri, pas un verdict : une photo très douce ou très sombre peut sortir comme floue. Filtre : toutes, modifiées, nombre d'étoiles minimum.
- Flèches : se déplacer. Entrée ou double-clic : ouvrir dans l'éditeur. Touches 1 à 5 : noter, 0 : effacer la note.
- Pastille bleue sous une vignette : la photo a des réglages.
- Catalogue : `~/.local/share/luminux/catalogue.db` (SQLite). Vignettes : `~/.cache/luminux/vignettes/`.
  Pour les RAW, la vignette vient de l'aperçu JPEG intégré au fichier, donc rapide.
- « IA sur la carte graphique » (activé par défaut) : les modèles IA locaux tournent sur la carte graphique
  par Vulkan (ONNX Runtime WebGPU : AMD, NVIDIA ou Intel, pas de CUDA). Au premier usage de chaque modèle, le
  résultat est comparé à celui du processeur ; s'il diffère, ce modèle reste sur le processeur. En cas
  d'échec, le processeur prend le relais. La ligne sous la case dit où a tourné le dernier calcul.
  `LUMINUX_AI_CPU=1` force le processeur.
- « Profil de l'écran » : aucun (sRGB, par défaut), celui du système (déclaré à colord, par exemple après
  un étalonnage avec DisplayCAL ou GNOME Couleur), ou un fichier `.icc`/`.icm`. L'image affichée est
  convertie vers ce profil ; l'export et l'histogramme restent en sRGB.

## Menus

En haut de la fenêtre : Fichier (nouveau, ouvrir, ajouter un dossier, importer des looks, exporter,
quitter), Édition (annuler, rétablir, copier/coller les réglages, auto, réinitialiser, historique,
préférences), Affichage (bibliothèque/édition, avant/après, zoom, disposition, plein écran), Bibliothèque
(actualiser, tri, filtre, copies virtuelles, afficher dans le dossier, informations), Accessibilité et Aide
(raccourcis clavier avec F1, mode d'emploi, journal des versions, à propos). Les raccourcis sont écrits à droite de chaque entrée.

La barre sous les menus ne garde que l'état de la vue, sans doublon des menus : bibliothèque ou édition, puis
tri, filtre et taille des vignettes (bibliothèque), ou avant/après, zoom et note (éditeur).

## Accessibilité

- Taille de l'interface : 100, 125, 150 ou 200 % (aussi Ctrl+plus et Ctrl+moins), gardée au prochain lancement.
- Contraste élevé : fond noir, texte blanc, contours nets, survol en jaune.
- Grandes zones de clic : boutons et cases plus grands et plus espacés, pour une main qui tremble ou un
  pointeur difficile à viser.
- Sans animations : les sections, le panneau et Avant/Après changent d'un coup, sans glissement ni fondu.
- Clavier : Tab et Maj+Tab passent d'un élément à l'autre, Espace ou Entrée l'active, Échap ferme la fenêtre
  ouverte (Préférences, Export, Raccourcis…). Liste complète : F1.
- Lecteur d'écran : menus, boutons, cases et curseurs sont exposés par AccessKit et lus par Orca (GNOME :
  Super+Alt+S). Les vignettes sont annoncées par leur nom. La photo elle-même n'est pas décrite.

## Éditeur

- Ouvrir : depuis la bibliothèque, bouton Ouvrir, Ctrl+O (fenêtre de vignettes, « Sélecteur du système… » en secours), ou glisser une photo dans la fenêtre.
- Flèches gauche/droite : photo précédente/suivante de la bibliothèque. G ou Échap : retour à la bibliothèque.
- Note : étoiles de la barre d'outils, ou touches 0 à 5.
- Zoom : molette autour du curseur, double-clic (ajuster ↔ 100 %), Ctrl+0 / Ctrl+1.
- Déplacement : glisser l'image.
- Avant/après : bouton « Avant » ou touche B pour voir l'original en entier. Bouton « Avant/Après » ou touche Y
  pour un rideau : l'original à gauche, le résultat à droite ; glisser la poignée pour déplacer la séparation.
- Curseur : double-clic pour le remettre à zéro.
- Courbe : clic pour ajouter un point, glisser pour le déplacer, double-clic pour le retirer.
- Annuler / rétablir : Ctrl+Z / Ctrl+Maj+Z (ou Ctrl+Y).
- Historique (dernière section du bloc Réglages, menu Édition ou H) : liste des modifications, clic sur une ligne pour revenir à cet état. Versions : donne un nom à l'état actuel (« noir et blanc », « chaud »…) pour y revenir plus tard ; elles sont gardées dans le fichier .luminux de la photo.
- Préférences (menu Édition ou Ctrl+,) : langue, côté de la barre d'outils (gauche ou droite), disposition « Réglages à part » (curseurs et courbes de l'autre côté de la photo, re-clic sur un outil pour replier son panneau), profil de l'écran, IA sur la carte graphique, masques IA en arrière-plan, réouverture de la dernière photo.
- « À propos » (menu Aide) : version, carte graphique, emplacement des données, bouton « Copier ces informations » pour signaler un bug.
- Vitesse : la photo suivante (dans le sens où tu avances) est décodée pendant que tu travailles, elle s'ouvre aussitôt. Les masques Sujet et Ciel des photos du dossier affiché dans la bibliothèque (les 500 plus récentes au plus) sont calculés en arrière-plan, seulement si leurs modèles sont déjà téléchargés, en pause pendant les chargements, exports et calculs IA. Changer de dossier fait passer ses photos en premier. La progression (faites/à faire) s'affiche en bas à droite. Les masques en cache sont compressés (environ 10 fois moins de place qu'avant la 0.11.1).
- Auto : menu Édition, « Réglages automatiques », règle exposition, blancs, noirs, ombres et vibrance d'après l'histogramme (annulable par Ctrl+Z).
- Recadrage : touche C ou bouton « Recadrer » (section Recadrage et redressement). Glisser un coin pour redimensionner, l'intérieur pour déplacer ; Entrée ou Échap pour valider. Ratios : libre, original, 1:1, 4:3, 3:2, 16:9, 5:4, case « Vertical » pour les inverser.
- Redresser : curseur en degrés (±45°) ; le cadre est réduit automatiquement pour ne jamais laisser de coins noirs.
- Détails : netteté (masque flou sur la luminance), réduction du bruit de luminance et de couleur (filtre bilatéral qui préserve les bords).
- Lumière : hautes lumières et ombres agissent par zones. Le poids vient de la luminosité moyenne autour du pixel,
  donc une zone sombre se débouche d'un bloc et garde son détail ; un ciel clair se tient sans griser les petits
  reflets d'une zone sombre. Un léger halo reste possible le long des bords très contrastés.
- Effets : clarté (contraste local), texture, grain.
- Virage partiel (teinte des ombres et des hautes lumières) et balance des couleurs (ombres, tons moyens, hautes lumières).
- Vignettage : assombrit (négatif) ou éclaircit (positif) les bords ; « Départ » règle où l'effet commence. Il suit le cadre recadré.
- Exporter : Ctrl+E ou menu Fichier, qui ouvre les options : format (JPEG, WebP, AVIF en 8 bits ; PNG, TIFF en 16 bits), espace couleur (sRGB, Display P3, Adobe RGB, profil ICC intégré), taille (originale ou bord long), netteté de sortie (écran ou impression), qualité (JPEG, WebP, AVIF), EXIF gardés (avec ou sans GPS ; pas dans le TIFF). WebP est environ un tiers plus léger que le JPEG à qualité égale ; AVIF est encore plus léger mais plus lent à écrire, et toujours en sRGB (l'encodeur n'intègre pas de profil ICC). Les mêmes options servent à l'export par lots de la bibliothèque.
- Auteur et copyright (fenêtre Exporter) : écrits dans les EXIF de chaque photo exportée (champs Artist et
  Copyright), même quand « Garder les métadonnées » est décoché.
- Filigrane (fenêtre Exporter) : un texte (blanc ou noir, avec une ombre légère pour rester lisible) ou un logo
  (PNG transparent conseillé), dans un coin ou au centre, avec sa largeur en pourcentage de la photo et son
  opacité. Il n'apparaît que dans le fichier exporté, pas dans l'éditeur.

Les réglages sont enregistrés automatiquement dans `photo.CR2.luminux` (JSON) à côté de la photo.
L'original n'est jamais modifié.

## Panneau de droite

- Une barre d'icônes à droite choisit l'outil affiché : Réglages, IA (outils de profondeur), Masques (réglages locaux), Recadrer, Looks,
  Ciel, Calques, Retouche, Objectif. Quitter un outil le referme (pinceau, recadrage, affichage du masque).
- Bouton à double chevron à côté du titre du panneau, touche P, menu Affichage ou re-clic sur l'icône de l'outil
  ouvert : masque le panneau pour voir la photo en grand. Une fine bande reste à sa place, avec le bouton qui le
  rouvre ; un clic sur une icône de la barre le rouvre aussi.
- Dans « Réglages », une seule section est ouverte à la fois ; le rond à droite d'un titre l'épingle pour la
  garder ouverte. La touche C ouvre directement « Recadrer ».
- Un point orange signale un réglage modifié, à gauche de son curseur et après le titre de sa section. La flèche
  circulaire du titre remet toute la section à zéro ; un double-clic sur un curseur le remet à sa valeur par défaut.
- Les curseurs de couleur ont une piste colorée (température du bleu au jaune, teinte du vert au magenta, TSL…).
- Les choix se font par boutons (formats de recadrage en tuiles, onglets Ombres / Tons moyens / Hautes lumières,
  modèles ComfyUI et Ollama…). Les listes déroulantes ne restent que pour les listes longues : objectifs Lensfun,
  packs de looks, plus de 8 modèles.
- Bibliothèque : tri et filtre en boutons ; clic sur une étoile pour n'afficher que les photos notées au moins
  autant (re-clic pour tout afficher).

## Réglages locaux

- Section « Réglages locaux » : « + Pinceau », « + Linéaire », « + Radial » (8 au maximum par photo).
- Clique sur un réglage pour le modifier : exposition, contraste, hautes lumières, ombres, température,
  teinte, saturation, clarté, appliqués seulement dans son masque. Case à cocher pour l'activer ou non.
- Pinceau : peindre sur la photo ; Alt (ou bouton « Gomme ») pour effacer ; [ et ] pour la taille.
- Dégradé linéaire : effet plein côté point plein, nul côté point creux. Radial : centre et deux poignées.
- « Voir le masque » (touche M) affiche le masque en rouge. « Inverser » applique l'effet à l'extérieur.
- Pendant un réglage local, le clic gauche sert à l'outil : déplacer la vue avec le clic droit glissé.

## Masques IA (ciel, sujet, arrière-plan)

- Dans « Réglages locaux » : « + Ciel IA », « + Sujet IA », « + Arrière-plan IA ». Le masque est détecté
  automatiquement, puis se règle comme un réglage local. Le pinceau l'étend, Alt (ou « Gomme ») le réduit.
- Premier usage : le modèle est téléchargé dans `~/.local/share/luminux/models` (sujet : IS-Net, 170 Mo,
  depuis GitHub ; ciel : skyseg, 170 Mo environ, depuis Hugging Face). Ensuite tout reste hors ligne.
- Le calcul tourne sur le processeur (ONNX Runtime, fourni dans l'AppImage), quelques secondes par photo.
  Le résultat est gardé dans `~/.local/share/luminux/masks` : rouvrir la photo est immédiat.
- Coller des réglages recopie les masques IA (sans les retouches au pinceau) ; ils sont recalculés sur
  chaque photo, y compris à l'export par lots.
- Masques de la scène : « + Personnes », « + Bâtiments », « + Végétation », « + Eau », « + Montagnes », « + Sol ».
  Un seul réseau (SegFormer-B0 entraîné sur ADE20K, 15 Mo, téléchargé depuis Hugging Face au premier usage)
  reconnaît 150 sortes d'éléments ; chaque bouton en regroupe plusieurs (Eau : mer, lac, rivière, piscine…).
- `luminux --mask-from <photo> <ciel|sujet> <masque.png>` remplace un masque par une image en niveaux de gris.

## Amélioration IA et Structure IA

- Onglet « IA », en tête de liste.
- Amélioration IA, « Accent » : un seul curseur qui débouche les ombres et tient les hautes lumières selon la
  luminosité de chaque zone, avec un peu de contraste local et de vibrance. « Ciel » : ciel plus dense et plus
  coloré, uniquement sur le ciel détecté par l'IA (masque de ciel, calculé au premier usage).
- Structure IA : renforce le détail moyen (ou l'adoucit en négatif) sans toucher au ciel ni aux aplats, pour
  ne pas faire ressortir le bruit. « Protéger le sujet » épargne aussi les personnes et visages (masque du
  sujet IA).
- Tout est calculé en direct sur la carte graphique, à l'écran comme à l'export.

## Débruitage IA

- Onglet « IA », section « Débruitage IA ». Le calcul passe par ton serveur ComfyUI avec un modèle de
  débruitage placé dans `ComfyUI/models/upscale_models`. Modèle conseillé : `scunet_color_real_psnr.pth`
  (https://github.com/cszn/KAIR/releases/download/v1.0/scunet_color_real_psnr.pth, 70 Mo environ). La version
  `scunet_color_real_gan.pth` donne un rendu plus net mais peut inventer du détail.
- « Débruiter » envoie la photo d'origine entière, par tuiles de 1024 px, une seule fois pour cette photo et ce
  modèle. Le résultat est gardé dans `~/.local/share/luminux/denoise`. Le curseur « Intensité » le mêle
  ensuite à la photo sans nouveau calcul ; l'image est refaite quand tu relâches le curseur.
- Le débruitage s'applique avant tout le reste (retouches, réglages, masques). L'export et l'export par lots
  le reprennent ; s'il manque en cache, ils le calculent.
- Seule la différence entre le résultat et la photo est ajoutée : les ombres gardent leur précision et les
  hautes lumières d'un RAW au-delà du blanc ne sont pas écrêtées.

## Visage IA et Peau IA

- Onglet « IA ». Chaque visage de la photo est détecté (YuNet), puis découpé en peau, yeux, lèvres et dents
  (BiSeNet entraîné sur CelebAMask-HQ). Les deux modèles (≈ 54 Mo) sont téléchargés depuis GitHub au premier
  usage ; le calcul prend environ une seconde et les masques sont gardés en cache.
- Visage IA : lumière du visage, yeux (netteté et éclat), blanc des yeux, saturation et rougeur des lèvres,
  blanchiment des dents.
- Peau IA : lissage (flou bilatéral à l'échelle du visage, le grain le plus fin est gardé) et réduction de la
  brillance. Agit sur la peau du visage, des oreilles et du cou.
- Plusieurs visages sont traités ensemble ; rien ne change ailleurs dans l'image.
- Forme : « Amincir le visage » resserre joues et mâchoire vers l'axe du visage, « Agrandir les yeux » grossit
  chaque œil. Les repères (position des yeux, écart, inclinaison de la tête) sont tirés du masque des yeux ;
  il faut donc les deux yeux visibles pour l'amincissement.

## Corps IA

- Onglet « IA », section « Corps IA ». Le curseur « Silhouette » affine (vers la droite) ou élargit (vers la
  gauche) le sujet, à partir du masque du sujet (IS-Net). La tête n'est pas touchée : la déformation
  commence sous le menton quand un visage est détecté, sinon sous le haut du sujet.
- Le curseur « Ventre » affine la taille autour du nombril (placé d'après le visage, sinon vers le milieu
  de la silhouette), sans toucher aux épaules ni aux jambes.
- Photo de groupe : chaque personne est traitée à part (quatre au plus, les plus grandes), avec son propre
  axe, et la déformation ne déborde jamais sur la voisine. Deux personnes qui se touchent forment une
  seule silhouette.
- Le fond proche du corps suit la déformation (comme un outil de fluidité) ; il n'est pas recréé.

## Outils IA : Relight, flou d'arrière-plan, atmosphère

- Onglet « IA » de la barre de droite. Les trois outils s'appuient sur une carte de profondeur calculée par
  Depth Anything V2 Small (99 Mo, téléchargé depuis GitHub au premier usage, puis hors ligne ; 1 à 2 s
  sur le processeur, résultat gardé comme les autres masques IA).
- Relight : éclaire (ou assombrit) le premier plan ; « Portée » règle jusqu'où la lumière porte, « Chaleur » sa
  couleur, « Assombrir l'arrière-plan » baisse le fond.
- Flou d'arrière-plan : flou progressif selon la distance au plan net. Mise au point « Auto » (le sujet au centre)
  ou « Choisir sur l'image » puis clic sur ce qui doit rester net. « Éclat des lumières » fait ressortir les
  points lumineux dans le flou, comme un vrai bokeh.
- Atmosphère : brume qui s'épaissit avec la distance, avec sa luminosité et sa chaleur.
- « Voir la profondeur » affiche la carte (blanc : proche, noir : loin), utile pour comprendre un résultat.

## Remplacement du ciel

- Section « Remplacement du ciel » : clique sur un ciel. Le ciel détecté par l'IA est remplacé, avec un accord
  automatique de luminosité sur l'ancien ciel. Réglages : opacité, position verticale, zoom, miroir,
  exposition du ciel, « Rééclairer la scène » (teinte la photo vers la couleur du nouveau ciel).
- Quatre ciels générés sont fournis pour commencer ; « Ajouter des ciels… » copie tes propres photos de ciel
  dans `~/.local/share/luminux/skies` (bouton « Dossier » pour l'ouvrir). Une image déposée directement dans ce
  dossier apparaît au prochain lancement.
- Le nouveau ciel est agrandi pour descendre jusqu'au bas du ciel détecté ; si « Position » le fait dépasser,
  son bord est prolongé en miroir (plus de traînées verticales).

## Calques

Onglet « Calques » : pose jusqu'à trois images sur la photo développée (texture, effet de lumière, logo,
autre photo). « Ajouter une image… » ouvre une fenêtre de vignettes (dernier dossier utilisé, sous-dossiers,
raccourcis Dossier personnel, Images, dossiers de la bibliothèque et disques montés, chemin cliquable ou à taper) ; « Sélecteur du système… » reste disponible. PNG (transparence gardée), JPEG, WebP et TIFF.

- La liste va du calque le plus haut au plus bas ; la case rend un calque visible ou non. « Monter »,
  « Descendre » et « Retirer » agissent sur le calque choisi.
- « Opacité » et « Fusion » : Normal, Produit, Écran, Assombrir, Incrustation, Lumière douce, Éclaircir.
- « Zone » : partout, ou seulement le ciel, le sujet ou le fond, d'après les masques IA de la photo.
- « Taille » 1 couvre tout le cadre ; plus petit, le calque se place avec « Position X » et « Position Y ».
  « Rotation » le fait tourner ; sur la photo, la poignée ronde au-dessus du calque fait de même (Maj : pas de 15°).
- « Sur la photo » : en mode « Déplacer », le calque se fait glisser à la souris (son cadre est dessiné) ;
  en mode « Lasso », on entoure la partie à garder, le reste devient transparent ; en mode « Gommer », le
  pinceau efface le calque (Alt rend ce qui a été gommé), avec « Taille du pinceau ».
  « Retirer la découpe » et « Retirer la gomme » rendent l'image entière.
- « Raccord avec la photo » : « Sujet seul (détourage IA) » ne garde que le sujet de l'image du calque
  (même modèle que le masque Sujet). « Harmoniser » rapproche le calque de la photo : teinte, voile des
  noirs et niveau des blancs, sans éclaircir un sujet sombre.
- « Fusionner dans une nouvelle image » enregistre la photo avec ses calques en PNG 16 bits
  (`nom_fusion.png`, dans le dossier Luminux) et l'ouvre : elle se retravaille ensuite comme une photo.
- Le calque garde le chemin de son image : si le fichier est déplacé, son nom passe en rouge et il disparaît.
- Les calques sont posés après tous les réglages : ils ne sont ni éclaircis ni recolorés par eux.

## Correction d'objectif

- Section « Objectif » : coche « Corriger l'objectif ». Le boîtier et l'objectif sont lus dans les EXIF et cherchés
  dans la base Lensfun intégrée (plus de 1 000 objectifs) ; la liste permet de choisir un autre objectif.
- Corrige la distorsion, l'aberration chromatique latérale et le vignetage, avec zoom automatique pour éviter
  les bords vides. Les profils dépendent de la focale (et de l'ouverture pour le vignetage), interpolés.
- Base de données Lensfun (https://lensfun.github.io), licence CC BY-SA 3.0, version 1.18.

## Retouche : correcteur de taches et gomme

- « Correcteur » : clic sur une tache (poussière, bouton). La source est choisie automatiquement dans le voisinage
  et raccordée en lumière et en couleur ; glisse le cercle pointillé pour en choisir une autre. `[` et `]` : taille.
- « Gomme IA » : peins l'objet à retirer (Alt pour enlever du masque), puis « Effacer ». La zone rouge montre
  exactement ce qui sera effacé (même tracé arrondi que le calcul). Le remplissage utilise
  le modèle LaMa (200 Mo environ, téléchargé depuis Hugging Face au premier usage, calcul sur le processeur).
  LaMa donne la forme d'ensemble, puis des morceaux de la photo prélevés autour du trou y sont recopiés du bord
  vers l'intérieur, pour garder le grain et la texture (vagues, herbe, feuillage) au lieu d'une zone lisse.
  Sans le modèle, ces morceaux seuls remplissent le trou et la barre d'état le signale.
- « Sélection » (gomme et génération) : « Pinceau » peint la zone ; « Lasso » entoure l'objet à main levée ;
  « Rectangle » se tire d'un coin à l'autre. Plusieurs zones s'ajoutent, Alt en retire une.
- « Remplissage » (gomme) : « IA (LaMa) » reconstruit la texture ; « Fond uni (rapide) » prolonge les couleurs
  voisines sans IA, instantané, adapté à un ciel, un mur ou une peau.
- Les retouches sont refaites à l'ouverture et à l'export ; le résultat de la gomme est gardé dans
  `~/.local/share/luminux/retouches`. « Retirer la dernière » et Ctrl+Z annulent.

## Moteur local (sans serveur)

Pour qui n'a pas de serveur ComfyUI : Préférences (Ctrl+,), « Moteur de génération IA », choisir
« Local (ce PC) ». Générer, Agrandir le cadre et Agrandir l'image passent alors par
[stable-diffusion.cpp](https://github.com/leejet/stable-diffusion.cpp), un programme C++ sans Python ni serveur,
qui utilise la carte graphique par Vulkan (AMD, NVIDIA, Intel) et le processeur en secours.

- « Télécharger » récupère le programme (37 Mo, binaire officiel Linux Vulkan) et les modèles conseillés :
  SD 1.5 inpainting (2,1 Go, 4 Go de mémoire vidéo suffisent) pour la génération, RealESRGAN x4 (64 Mo) pour
  l'agrandissement. Tout va dans `~/.local/share/luminux/sdcpp`.
- « Autre modèle… » prend un autre modèle de génération (.safetensors, .ckpt ou .gguf : SD 1.5, SDXL, de
  préférence « inpainting »). D'autres modèles d'agrandissement (.pth de type ESRGAN) peuvent être déposés
  dans `sdcpp/models` ; ils apparaissent alors dans les Préférences.
- Les sections concernées portent l'étiquette « Local », verte quand le moteur et le modèle sont là.
- Le débruitage IA reste réservé à ComfyUI : sa section est grisée en mode Local.
- Ollama n'est pas nécessaire : ses options (libérer la carte, traduire la consigne) sont repliées sous
  « Ollama (facultatif) ». Sans Ollama, la consigne part telle quelle.
- Un modèle plus léger veut dire des générations moins détaillées que SDXL sur un serveur : le moteur local
  vise les PC sans grosse carte graphique. Sans carte graphique reconnue, tout passe au processeur : compter
  plusieurs minutes pour un agrandissement.

## Ce qui demande un serveur ComfyUI

Tout fonctionne sans serveur, sauf quatre outils qui passent par **ComfyUI**. C'est un serveur d'IA
gratuit qui s'installe à part, sur ce PC ou sur une autre machine du réseau, avec une carte AMD (ROCm) ou
NVIDIA. Dans l'appli, ces sections portent l'étiquette « ComfyUI », qui devient verte une fois le serveur
connecté. Les autres outils IA (masques, ciel, profondeur, visage, gomme) tournent sur ce PC et téléchargent
leur modèle tout seuls.

| Outil | Modèle à placer dans ComfyUI | Dossier |
|---|---|---|
| Retouche → Générer, IA → Agrandir le cadre | [sd_xl_base_1.0_inpainting_0.1.safetensors](https://huggingface.co/wangqyqq/sd_xl_base_1.0_inpainting_0.1.safetensors) | `models/checkpoints` |
| IA → Agrandir l'image | [RealESRGAN_x4plus.pth](https://github.com/xinntao/Real-ESRGAN/releases/download/v0.1.0/RealESRGAN_x4plus.pth) | `models/upscale_models` |
| IA → Débruitage IA | [scunet_color_real_psnr.pth](https://github.com/cszn/KAIR/releases/download/v1.0/scunet_color_real_psnr.pth) | `models/upscale_models` |

1. Installe ComfyUI : https://github.com/comfyanonymous/ComfyUI (version portable ou `pip`, voir sa page).
   Pour qu'une autre machine du réseau puisse le joindre, lance-le avec `--listen`.
2. Place les modèles dans les dossiers indiqués, puis relance ComfyUI.
3. Dans Luminux, ouvre l'une de ces sections, indique l'adresse du serveur (`http://127.0.0.1:8188` si ComfyUI
   tourne sur ce PC), puis clique sur « Connecter ». Si un modèle manque, la section le dit et donne le lien.
4. Facultatif : Ollama (https://ollama.com) traduit en anglais les invites écrites en français.
5. Pour vérifier un serveur sans ouvrir l'appli : `COMFY=http://machine:8188 python3 tools/test_comfyui.py`.

Un résultat déjà calculé (génération, débruitage) est gardé en cache : la photo s'ouvre et s'exporte ensuite
sans le serveur.

## Génération avec ComfyUI

- Retouche → « Générer » : peins une zone, décris ce qui doit y apparaître, puis « Générer ». La zone et son
  entourage partent vers ton serveur ComfyUI (par défaut `http://127.0.0.1:8188`, ce PC ; modifiable), le résultat
  est fondu dans la photo. « Autre variante » relance avec une autre graine ; « Retirer la dernière » annule.
- Le modèle se choisit parmi les checkpoints du serveur. Un modèle d'inpainting (nom contenant « inpaint »)
  est pris par défaut s'il existe et donne les meilleurs raccords. Avec un autre modèle, « Force » règle
  à quel point la zone d'origine est remplacée (1 = entièrement).
- Réglages devinés d'après le nom du modèle : SDXL / Turbo / Lightning en 1024 px, les autres en 640 px ;
  8 étapes pour les modèles rapides (Turbo, Lightning, LCM, Hyper), 28 sinon.
- Le résultat est gardé dans `~/.local/share/luminux/retouches` : rouvrir ou exporter la photo ne relance
  pas le serveur.
- « Traduire l'invite en anglais (Ollama) », activé par défaut : tu écris en français, un modèle d'Ollama traduit
  l'invite juste avant l'envoi (le modèle d'image ne comprend bien que l'anglais). « Auto » prend le plus petit
  modèle de texte installé ; la traduction utilisée s'affiche dans la barre d'état.
- « Libérer la carte graphique (Ollama) » : si Ollama tourne sur le même serveur, Luminux décharge ses modèles
  avant chaque génération, puis demande à ComfyUI de libérer sa mémoire une fois l'image reçue. « État » montre
  les modèles chargés et la mémoire vidéo qu'ils occupent ; « Décharger maintenant » vide la carte à la main.
  Ollama recharge son modèle à la requête suivante, qui est donc plus lente. Désactivé par défaut.

## Détourer : PNG au fond transparent

- Onglet « IA », section « Détourer (fond transparent) ». Le sujet principal est détecté par l'IA (même masque
  que « Sujet IA ») ; « Voir le détourage » montre en damier ce qui deviendra transparent.
- « Exporter en PNG transparent… » écrit un PNG 16 bits avec transparence, avec tous tes réglages.
- Sans fenêtre : `luminux --detourer photo.jpg sortie.png`.
- Le détourage suit le sujet principal : sur un paysage sans sujet net, le résultat est flou (pas encore de
  retouche du masque au pinceau dans cette section).

## Agrandir le cadre (GenExpand) et agrandir l'image (Upscale)

- Onglet « IA », sections « Agrandir le cadre » et « Agrandir l'image ». Les deux passent par ton serveur
  ComfyUI (même serveur et mêmes options mémoire vidéo / traduction que « Générer »).
- Le résultat est un **nouveau fichier** dans `~/Images/Luminux` (`photo_etendu.png`, `photo_x2.png`), ouvert
  aussitôt ; l'original n'est pas touché. Le calcul porte sur la photo telle que tu la vois (réglages et
  recadrage compris).
- Agrandir le cadre : un format visé (1:1, 4:5, 3:2, 16:9, 9:16), « Tout autour », ou une marge par côté.
  Le cadre final est encadré en jaune sur l'image. Invite facultative (vide : la suite naturelle de la
  photo). La photo garde sa pleine résolution ; les bords ajoutés sont calculés vers 1024 px puis agrandis,
  donc plus doux que la photo sur une grande image.
- Agrandir l'image : × 2, × 3 ou × 4 avec un modèle d'agrandissement de ComfyUI. Il en faut un dans
  `ComfyUI/models/upscale_models` (par exemple RealESRGAN_x4plus.pth ou 4x-UltraSharp.pth), puis « Connecter ».
  La photo part en tuiles de 1024 px (raccords recouverts) et l'avancement s'affiche tuile par tuile.
  Taille finale limitée à 150 Mpx : au-delà, le bouton est grisé et le panneau le dit.

## Nouveau document

- « Nouveau… » (Ctrl+N), dans la bibliothèque comme dans l'éditeur : choisis une taille (préréglages ou libre)
  et une couleur de fond. Le document est créé dans `~/Images/Luminux` (Document_1.png, Document_2.png…),
  ajouté à la bibliothèque et ouvert dans l'éditeur avec tous les outils.
- « Générer l'image avec ComfyUI » : décris l'image, elle est générée sur tout le document (calcul autour de
  1024 px puis agrandissement à la taille choisie). « Autre variante » dans Retouche → Générer pour une autre version.

## Bibliothèque : sélection, copier/coller, export par lots

- Ctrl+clic, Maj+clic, Ctrl+A pour sélectionner plusieurs photos ; Échap pour désélectionner.
- Clic droit sur une vignette : « Créer une copie virtuelle » (même photo, réglages indépendants dans `photo.jpg#2.luminux`, sans fichier image en plus ; badge « copie »), « Supprimer la copie virtuelle », « Ouvrir l'emplacement du fichier » (le gestionnaire de fichiers sélectionne la photo, sinon ouvre son dossier).
- Mots-clés (colonne de gauche) : saisis-en un ou plusieurs, séparés par des virgules, pour la photo ou la sélection ; « mot × » le retire. Clic sur un mot-clé de la liste pour n'afficher que ses photos. Ils sont gardés dans le catalogue.
  Les notes (0 à 5) s'appliquent à toute la sélection.
- « Copier les réglages » (menu Édition, Ctrl+Maj+C) puis « Coller les réglages » (Ctrl+Maj+V) : tout sauf le recadrage et les masques
  dessinés, propres à chaque photo. Fonctionne aussi dans l'éditeur.
- « Exporter (N)… » (menu Fichier, Ctrl+E) : exporte la sélection avec ses réglages dans un dossier, au format choisi.
  Un fichier existant n'est jamais écrasé (suffixe _1, _2…). En ligne de commande :
  `luminux --batch <dossier> <jpg|webp|avif|png|tif> <photos…>`.
- Option « Rouvrir la dernière photo au démarrage » dans les Préférences.

## Langue de l'interface

- Préférences, « Langue » : français ou anglais, intégrés. Au premier lancement, l'appli prend
  le français si le système est en français, l'anglais sinon.
- Autre langue, à la main : « Modèle à traduire… » enregistre le fichier anglais (JSON : texte français à
  gauche, traduction à droite). Traduis les textes de droite, mets le nom de la langue dans `_name`, puis
  « Importer… ». Garde tels quels les trous entre accolades (`{n}`, `{e:#}`).
- Autre langue, par l'IA : écris le nom de la langue (« Deutsch », « Español »…) puis « Traduire ». Ollama
  (serveur et modèle réglés dans Retouche → Générer ; en « Auto », le plus petit modèle de texte, les modèles
  de vision étant écartés) traduit l'interface par lots ; un texte mal rendu reste
  en anglais. Le fichier obtenu est dans `~/.local/share/luminux/lang/`, modifiable avec gedit.
- Les messages d'erreur techniques (serveur injoignable, fichier illisible…) restent en français.

## Looks (LUT et presets)

- Bouton « Importer… » du panneau Looks (ou glisser les fichiers sur la fenêtre, ou `luminux --import <fichiers>`).
  Formats : LUT 3D `.cube`, presets Lightroom `.xmp` et `.lrtemplate`, presets Luminar `.lnpc`, `.lnp`, `.lmps`,
  et les `.zip` de pack tels quels (même imbriqués). « Dossier… » importe tout un dossier de packs.
- Chaque pack apparaît dans la liste déroulante, avec un aperçu de chaque look sur ta photo. Un clic l'applique
  (le recadrage et la réduction du bruit de la photo sont conservés). Ctrl+Z pour revenir en arrière.
- Les LUT donnent exactement le rendu prévu ; curseur « Intensité LUT » pour doser.
- Les presets Lightroom et Luminar sont convertis vers les réglages de Luminux : rendu proche, pas identique
  (les formules d'Adobe et de Skylum ne sont pas publiques). Les réglages sans équivalent (outils IA de Luminar,
  Dehaze, calibration…) sont listés après l'import et dans l'infobulle de chaque look.
- « Enregistrer… » crée un look à partir des réglages actuels, dans le pack « Mes looks ».
  Clic droit sur un look pour le supprimer.
- Les looks sont copiés dans `~/.local/share/luminux/looks/` : tes fichiers d'origine ne sont plus nécessaires.
  « Retirer » supprime le pack de Luminux seulement.

## Limites connues

- Moteur local : l'agrandissement a été essayé en vrai (processeur seul) ; la génération n'a pas pu l'être
  pendant le développement (modèle sur Hugging Face, inaccessible depuis l'environnement de test).
- Débruitage IA : il passe par ComfyUI (pas de calcul local) et travaille sur une image 8 bits ; essayé
  seulement contre un faux serveur pendant le développement.
- La vignette d'une photo modifiée hors de l'éditeur (premier scan) est calculée sur l'aperçu intégré du RAW : ses couleurs peuvent différer légèrement de l'éditeur. Elle est recalculée exactement dès que tu quittes la photo dans l'éditeur.
- Profil de l'écran : seule l'image en cours d'édition est convertie ; les vignettes de la bibliothèque et
  l'interface restent en sRGB.
- IA sur la carte graphique : la vitesse réelle n'a pas été mesurée pendant le développement (pas de vraie
  carte graphique dans l'environnement de test, seulement un pilote Vulkan logiciel). Sujet, profondeur,
  détection et découpage des visages ont donné les mêmes masques qu'au processeur ; ciel, gomme LaMa et
  masques de la scène n'ont pas pu être essayés (modèles hébergés sur Hugging Face, inaccessible), mais la
  comparaison automatique au premier usage les renvoie au processeur s'ils diffèrent.
- Génération, agrandissement du cadre et de l'image (ComfyUI) : vérifiés sur un vrai ComfyUI 0.40 avec le
  script tools/test_comfyui.py. Agrandir le cadre pré-remplit les bandes avec la photo étirée et floutée
  (denoise 0,85) : avec des bandes grises, SDXL inpainting dessinait une petite image encadrée. Le réglage
  retenu vient d'un essai sur une seule photo ; l'appli elle-même n'a été essayée que contre un faux serveur.
- Masques de la scène : le réseau n'a pas pu être essayé pendant le développement (Hugging Face inaccessible
  depuis l'environnement de test) ; il travaille sur une image réduite à 512 px, les contours sont ensuite
  affinés sur la photo.
- Outils de profondeur : la carte est relative (pas en mètres) et un peu molle sur les contours fins
  (feuillages, cheveux) ; le flou peut y laisser un léger liseré.
- Visage, Peau et Corps IA : les visages très petits (moins d'une vingtaine de pixels) ou de profil marqué peuvent ne
  pas être détectés. Le rayon du lissage se règle sur la surface de peau totale : avec beaucoup de visages
  il est un peu plus grand. Corps IA sépare les personnes d'après le masque du sujet : des personnes
  qui se touchent ou se chevauchent comptent pour une, et l'effet est faible si le sujet touche les bords.
  La position du ventre est estimée (proportions moyennes), pas détectée. Les tracés de pinceau ne
  suivent pas ces déformations (ils suivent la correction d'objectif).

## AppImage (sans compiler)

L'AppImage de chaque version est dans les Releases du dépôt GitHub (construite par GitHub Actions à
chaque tag `v*`, ou à la main depuis l'onglet Actions, « Release AppImage »).

    chmod +x Luminux_IA-x86_64.AppImage
    ./Luminux_IA-x86_64.AppImage

Pour la reconstruire après une modification du code : `packaging/build-appimage.sh`
(nécessite `appimagetool`, voir l'en-tête du script).

## Mesurer les temps

    LUMINUX_DEBUG=1 ./Luminux_IA-x86_64.AppImage

affiche dans le terminal le temps de décodage, de conversion et d'envoi au GPU de chaque photo ouverte.
