# TokenScope

Application Windows native en **Rust**, avec interface egui/OpenGL. Aucun Node.js, Electron, navigateur embarqué ou serveur web pour l’interface.

## Démarrer

Télécharger `TokenScope-Windows-x64.zip` depuis la [dernière release](https://github.com/brandonviry/TokenScope/releases/latest), extraire son contenu puis double-cliquer sur **TokenScope.exe**. Le premier scan des journaux s’effectue en arrière-plan. L’application reste ouverte pour actualiser les données.

Les connexions Codex et Claude peuvent être détectées dans leurs fichiers de session habituels. La clé **Ollama Cloud** peut être retrouvée automatiquement dans la configuration d’OpenCode. Les fichiers d’origine sont lus sans être modifiés.

Depuis la version **1.1.0**, **Antigravity · Gemini** est également ajouté automatiquement au tableau de bord. Votre connexion Google existante est détectée ; le forfait Google AI Pro/Ultra est affiché lorsqu’il est communiqué par Google. Les configurations de la version précédente sont conservées, avec une sauvegarde avant l’ajout du connecteur.

Les jauges affichent la consommation du quota, le pourcentage restant, le délai avant la prochaine réinitialisation et sa date en heure locale. Chaque relevé porte sa date de collecte. Une erreur de connexion conserve le dernier relevé ; une échéance dépassée reste « à confirmer » tant que le fournisseur ne l’a pas actualisée.

## Tableau de bord et historique

- Filtres : aujourd’hui, semaine civile, mois civil et tout l’historique enregistré.
- Graphiques : 7 derniers jours, 12 semaines ou 12 mois ; tokens connus, requêtes ou valeur d’usage en USD.
- Historique par fournisseur et modèle, avec entrées, sorties, cache, requêtes et coût lorsqu’il est communiqué.
- Export CSV de l’historique filtré, import CSV/JSONL et saisie manuelle.
- Fournisseurs supplémentaires et budgets personnels en tokens, USD ou crédits, par jour, semaine, mois ou fenêtres fixes de 5 heures.
- Seuil d’alerte réglable. `F5` déclenche une actualisation.

Les filtres calendaires utilisent l’heure locale de Windows. Les historiques distants Ollama et Anthropic sont fournis sous forme d’agrégats journaliers UTC : leur date correspond au début du bucket UTC, affiché en heure locale. La ventilation précise d’un agrégat entre deux jours locaux n’est pas disponible. Les comptes à rebours des quotas utilisent directement les échéances du fournisseur.

## Sources prises en charge

| Source | Consommation | Limites et réinitialisations |
| --- | --- | --- |
| Codex | Journaux `CODEX_HOME/sessions` et `archived_sessions`, sinon `~/.codex` | Compte connecté ; repli sur les snapshots des journaux |
| Claude / Claude Code | Journaux de Claude Code dans `CLAUDE_CONFIG_DIR/projects`, sinon `~/.claude/projects` | Quota d’abonnement partagé, lu avec la session Claude Code |
| Google Antigravity / Gemini | Imports/saisies si un historique de tokens est disponible ; aucun compteur n’est déduit d’une fraction de quota | Limites actives par modèle, échéances et forfait Google AI lus avec la session Antigravity existante |
| Ollama Cloud | API officielle `/api/usage`, historique de 30 jours puis conservation locale des buckets collectés | API officielle `/api/balance` : anciennes fenêtres session/semaine ou allocation mensuelle et crédits achetés |
| Anthropic API | API Analytics de l’organisation, historique de 90 jours | Budgets personnels configurables ; nécessite une clé Admin/Analytics |
| Autres fournisseurs | Journaux au format JSONL commun ou import CSV | Budgets personnels et relevés manuels |
| Ollama local, optionnel | Collecteur Rust intégré ou import | Budgets personnels ; pas de quota Cloud déduit des appels locaux |

**Ollama Cloud :** les forfaits qui ne renvoient que des compteurs de requêtes affichent des tirets pour les tokens dans l’historique. Les requêtes ne sont jamais transformées artificiellement en tokens. Le graphique « Requêtes » permet d’en suivre l’activité. Les forfaits avec allocation mensuelle affichent les crédits inclus et leur date de renouvellement, ainsi que le solde acheté séparément.

**Claude :** Claude et Claude Code partagent le même quota d’abonnement. La vue d’ensemble les regroupe et compte cette limite une seule fois. Les tokens du chat Claude ne sont pas présents dans les journaux de Claude Code ; ils nécessitent un import ou une saisie manuelle.

**Tokens et coût :** le cache est déjà inclus dans les tokens d’entrée. Le raisonnement est déjà inclus dans les sorties Codex. Aucun de ces compteurs n’est additionné une seconde fois. Aucun tarif de modèle n’est inventé. Les coûts affichés proviennent de données du fournisseur ou de vos relevés ; une valeur d’usage couverte par un abonnement n’est pas une facture supplémentaire.

## Connexion Antigravity / Gemini

La connexion automatique lit uniquement la session Google d’Antigravity, notamment l’entrée `gemini:antigravity` du coffre Windows. Il n’est pas nécessaire de saisir une clé API Gemini AI Studio. Une clé AI Studio correspond à une facturation API distincte de l’abonnement Google AI.

Dans **Connexions → Antigravity · Gemini**, laisser le chemin de session vide pour la détection automatique. Un fichier JSON de session Antigravity peut aussi être sélectionné. Les fichiers Gemini CLI ne sont pas recherchés automatiquement, afin de ne pas confondre les deux produits.

Quand la session du coffre Windows expire, TokenScope peut demander au client natif `agy` installé de l’actualiser via sa commande de liste des modèles. Cette commande n’envoie pas de prompt et ne génère pas de tokens. Aucun refresh token n’est recopié dans les données TokenScope. Si le client ne peut pas renouveler la session, ouvrez Antigravity pour vous reconnecter, puis actualisez.

Les points de lecture `fetchAvailableModels` et `loadCodeAssist` renvoient les modèles et quotas actuellement exposés à ce compte. La mention **quota actif** est volontaire : une échéance seule ne permet pas d’identifier avec certitude une fenêtre de 5 heures ou une limite hebdomadaire. Les fenêtres non détaillées, les tokens consommés et les crédits non communiqués restent inconnus. Aucun pourcentage hebdomadaire n’est inventé. Plusieurs modèles peuvent partager un même quota.

Les crédits Google AI sont affichés seulement si le compte renvoie explicitement un solde `GOOGLE_ONE_AI`. La valeur zéro est alors distinguée de l’absence de solde. Ces points de lecture du client Antigravity peuvent évoluer, comme ceux de Codex et Claude.

## Connexion Ollama Cloud

La recherche de clé suit cet ordre :

1. Variable configurée, `OLLAMA_API_KEY` par défaut, dans le processus ou les variables Windows utilisateur/système.
2. Clé enregistrée pour ce compte dans le **coffre d’identifiants Windows** depuis l’application.
3. Clé API Ollama existante dans les emplacements habituels d’OpenCode, notamment `~/.local/share/opencode/auth.json`.

Pour utiliser une autre clé : ouvrir **Connexions → Ollama Cloud**, saisir la clé dans le champ masqué, puis cliquer sur **Enregistrer et connecter**. Les clés ne sont pas enregistrées dans `config.json`, les exports ou les diagnostics. Le nom de la variable peut être modifié sans saisir sa valeur dans la configuration.

L’application effectue uniquement des lectures de consommation et de solde. Elle ne lance aucune génération et ne rachète aucun crédit. Si une session Codex/Claude expire, reconnectez-vous dans son client officiel ; TokenScope relira les identifiants au prochain rafraîchissement.

## Ajouter un fournisseur ou un budget

Dans **Connexions**, cliquer sur **Ajouter**, choisir le type de source puis son chemin ou sa connexion. Un dossier de journaux est parcouru récursivement ; plusieurs chemins peuvent être indiqués, un par ligne.

Un budget personnel est indépendant de l’abonnement du fournisseur. Les semaines commencent le lundi et les mois le premier jour. Les fenêtres de 5 heures ont un ancrage fixe modifiable. Un relevé manuel permet de saisir une consommation déjà observée et son échéance exacte. Après cette échéance, il reste marqué expiré jusqu’à sa mise à jour ou sa désactivation.

Pour les budgets en USD/crédits, seules les valeurs connues des relevés sont additionnées. Les fichiers d’exemple ne sont **jamais chargés automatiquement**.

Format JSONL commun, un objet par ligne :

```json
{"id":"requete-unique-001","provider":"mon-fournisseur","timestamp":"2026-11-02T08:30:00Z","model":"mon-modele","input_tokens":1200,"output_tokens":300,"cached_tokens":800,"cost_usd":0.01,"credits":null,"request_count":1,"tokens_reported":true,"source":"Export personnel"}
```

`id`, `provider` et `timestamp` sont requis. Les compteurs absents valent zéro ; les coûts/crédits absents restent inconnus. `tokens_reported:false` signale que le fournisseur ne fournit pas de compteur de tokens. Lors d’un import, le fournisseur sélectionné reçoit les relevés. Les identifiants stables évitent les doublons lors des réimports. Pour préparer un CSV, utiliser l’en-tête de `examples/consommation.csv` ou un export de l’application.

## Ollama local, si nécessaire

Ce collecteur est optionnel et n’est pas nécessaire pour Ollama Cloud.

1. Ajouter un fournisseur **Ollama local** dans Connexions.
2. Dans Réglages, activer le collecteur et enregistrer.
3. Garder Ollama démarré sur `http://127.0.0.1:11434`.
4. Configurer le client local pour appeler `http://127.0.0.1:11435`.

Le relais transmet les réponses en streaming et enregistre seulement les métriques numériques reçues à la fin. Les prompts et les réponses ne sont pas sauvegardés. Les appels directs au serveur Ollama ne traversant pas ce relais ne peuvent pas être comptés. Pour un client compatible OpenAI en streaming, demander `stream_options.include_usage=true`.

## Stockage et actualisation

Le chemin exact est affiché dans **Réglages → Données de l’application**. Sous Windows, il se trouve généralement dans `%LOCALAPPDATA%\TokenScope\TokenScope\data`.

- `config.json` : sources, budgets et préférences ; `config.backup.json` conserve la configuration précédente.
- `events.jsonl` : imports et saisies manuelles.
- `ollama.jsonl` : métriques du collecteur local optionnel.
- `quotas.json` : dernières limites connues, sans identifiants secrets.
- `remote-events.json` : historique numérique des APIs distantes collecté au fil du temps.
- `diagnostic.json` : rapport produit par l’option `--check`.

Les journaux sont relus de manière incrémentale. Le scan local se fait toutes les 60 secondes par défaut, les quotas distants toutes les 3 minutes. Une actualisation manuelle peut relancer une lecture distante après 20 secondes. Les compteurs temporels se redessinent chaque seconde ; l’application n’exécute pas de boucle de rendu permanente.

Les lectures de quota Codex et Claude reposent sur les points de lecture de leurs clients et peuvent évoluer. Les erreurs HTTP, sessions expirées, sources manquantes et lignes invalides sont signalées dans l’interface. Les derniers relevés restent datés.

## Compiler et vérifier

Rust 1.88 ou plus récent, avec les outils de compilation C++ MSVC pour Windows.

```powershell
cargo test --release --locked
cargo clippy --release --all-targets -- -D warnings
cargo build --release --locked
```

L’exécutable produit est `target\release\tokenscope.exe`. `Compiler.ps1` exécute ces vérifications puis copie `TokenScope.exe` et son empreinte SHA-256 dans `dist/`. Ce dossier contient des livrables locaux et est exclu de Git. Le script ne met pas à jour les archives ZIP existantes : elles doivent être recréées après une nouvelle compilation.

Options de diagnostic :

```powershell
.\dist\TokenScope.exe --check
.\dist\TokenScope.exe --data-dir .\profil-test
.\dist\TokenScope.exe --screenshot .\capture.png
.\dist\TokenScope.exe --screenshot .\historique.png --page history
.\dist\TokenScope.exe --provider antigravity
```

Le mode capture utilise le moteur de rendu natif, attend la fin de la première collecte, sauvegarde l’image et ferme sa fenêtre. `--page` accepte `history`, `connections` et `settings`. Le rapport `--check` est écrit dans le dossier de données même si le binaire Windows n’a pas de console.

## Références des intégrations

- [Ollama : spécification officielle Usage et Balance](https://github.com/ollama/ollama/blob/main/docs/openapi.yaml)
- [Ollama Cloud et authentification par clé](https://docs.ollama.com/cloud)
- [Ollama : métriques de tokens et de cache](https://docs.ollama.com/api/usage)
- [Codex App Server : schéma des limites et réinitialisations](https://learn.chatgpt.com/docs/app-server)
- [Anthropic : Usage and Cost Analytics API](https://platform.claude.com/docs/en/manage-claude/usage-cost-analytics-api)
- [Google Antigravity : forfaits et fenêtres de quota](https://antigravity.google/docs/plans)
- [Google Antigravity : affichage des quotas du client](https://antigravity.google/docs/cli/commands/usage)


## Organisation du dépôt

Le code Rust se trouve dans `src/`, les exemples d’import dans `examples/` et le script de compilation à la racine. `target/` contient les fichiers générés par Cargo. `dist/` contient les livrables locaux lorsqu’ils ont été créés ou téléchargés. Ces deux dossiers ne sont pas suivis par Git. Le dossier `qa/` contient des scripts de diagnostic documentés dans son README ; leurs profils, captures et résultats locaux sont exclus de Git. Le dossier `.research/`, composé de références externes locales, est également exclu.

`SHA256SUMS.txt` est une empreinte publique permettant de contrôler l’intégrité du fichier, pas une clé secrète ni une signature attestant son origine. Dans PowerShell, comparez-la avec `(Get-FileHash .\dist\TokenScope.exe -Algorithm SHA256).Hash`.
