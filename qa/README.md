# Contrôles manuels de TokenScope

Ces scripts Windows sont des outils de diagnostic **en ligne**, destinés à une exécution manuelle sur une machine de test. Ils ne font pas partie des tests automatisés `cargo test` et ne conviennent pas à une CI sans adaptation. Ils exigent Python 3, une connexion réseau et les comptes et droits nécessaires. Les réponses des services et les formats de session peuvent évoluer.

## Vérification et captures

Compiler d’abord le binaire attendu, puis lancer :

```powershell
cargo build --release --locked
python qa/smoke.py
```

Le script utilise `target/release/tokenscope.exe` et suppose que les sources Codex, Claude, Claude Code, Ollama Cloud et Antigravity sont configurées et accessibles sur la machine, avec des événements existants. Il effectue des connexions réelles, vérifie certains résultats, produit des captures et écrit ses profils et son rapport dans `qa/`. Un échec peut donc refléter une configuration absente ou un service indisponible, et non nécessairement un défaut de l’application. Les profils et captures portant les mêmes noms sont écrasés ou mis à jour.

## Diagnostic Antigravity

```powershell
python qa/antigravity_probe.py
```

Ce script lit la session `gemini:antigravity` du coffre d’identifiants Windows et utilise son jeton pour interroger des points de terminaison Google internes. Il affiche des informations de forfait et de quota et écrit une sélection de réponses dans `qa/antigravity-live-response.json`. Il nécessite une session existante et valide ; ces API internes peuvent changer. Exécutez-le uniquement sur un compte que vous êtes autorisé à utiliser.

## Données locales

Les profils, captures, rapports et réponses générés dans ce dossier sont ignorés par Git : ils peuvent contenir des données de compte ou de consommation. Vérifiez leur contenu avant de les partager. Seuls les scripts Python à la racine de `qa/` et ce README sont destinés à être suivis.
