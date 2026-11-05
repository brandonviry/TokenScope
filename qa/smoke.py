import json
import pathlib
import subprocess
import time

ROOT = pathlib.Path(__file__).resolve().parent.parent
EXE = ROOT / 'target/release/tokenscope.exe'
QA = ROOT / 'qa'
LIVE = QA / 'live-profile'
CREATE_NO_WINDOW = 0x08000000

def run(*args):
    start = time.perf_counter()
    process = subprocess.run([str(EXE), *map(str, args)], cwd=ROOT,
                             timeout=95, capture_output=True,
                             creationflags=CREATE_NO_WINDOW)
    if process.returncode:
        raise RuntimeError(f'Application exited with code {process.returncode}')
    return round(time.perf_counter() - start, 3)

elapsed = run('--check', '--data-dir', LIVE)
diagnostic = json.loads((LIVE / 'diagnostic.json').read_text(encoding='utf-8'))
providers = {p['id']: p for p in diagnostic['providers']}
assert diagnostic['events'] > 0
assert not diagnostic['warnings'], diagnostic['warnings']
for provider_id in ['codex', 'claude', 'claude-code', 'ollama-cloud', 'antigravity']:
    assert providers[provider_id]['live'], (provider_id, providers[provider_id]['status'])
assert providers['claude']['shared_group'] == providers['claude-code']['shared_group']
assert providers['ollama-cloud']['usage_note']
print(json.dumps({'check_seconds': elapsed, 'events': diagnostic['events'],
                  'tokens': diagnostic['tokens'], 'connected': list(providers)}, ensure_ascii=False), flush=True)

shots = {}
shots['overview'] = run('--data-dir', LIVE, '--screenshot', QA / 'overview.png')

cloud = QA / 'cloud-profile'
cloud.mkdir(exist_ok=True)
config = {'schema_version': 2, 'providers': [{'id': 'ollama-cloud', 'name': 'Ollama Cloud',
                        'kind': 'OllamaCloud', 'online': True,
                        'key_env': 'OLLAMA_API_KEY', 'color': [120, 174, 250]}]}
(cloud / 'config.json').write_text(json.dumps(config), encoding='utf-8')
shots['connections'] = run('--data-dir', cloud, '--page', 'connections', '--screenshot', QA / 'connections.png')
config['providers'][0]['online'] = False
(cloud / 'config.json').write_text(json.dumps(config), encoding='utf-8')
shots['history'] = run('--data-dir', cloud, '--page', 'history', '--screenshot', QA / 'history.png')

settings = QA / 'settings-profile'
settings.mkdir(exist_ok=True)
(settings / 'config.json').write_text(json.dumps({'schema_version':2, 'providers': []}), encoding='utf-8')
shots['settings'] = run('--data-dir', settings, '--page', 'settings', '--screenshot', QA / 'settings.png')
google = QA / 'antigravity-profile'
google.mkdir(exist_ok=True)
(google / 'config.json').write_text(json.dumps({'schema_version':2,'providers':[{'id':'antigravity','name':'Antigravity · Gemini','kind':'Antigravity','online':True,'color':[138,180,248]}]}),encoding='utf-8')
shots['antigravity'] = run('--data-dir', google, '--provider', 'antigravity', '--screenshot', QA / 'antigravity.png')
shots['antigravity-connections'] = run('--data-dir', google, '--provider', 'antigravity', '--page', 'connections', '--screenshot', QA / 'antigravity-connections.png')
for page in shots:
    assert (QA / f'{page}.png').stat().st_size > 10_000
report = {'live_check_seconds': elapsed, 'events': diagnostic['events'],
          'providers_connected': list(providers), 'render_seconds': shots,
          'screenshots': [f'{page}.png' for page in shots]}
(QA / 'smoke-result.json').write_text(json.dumps(report, indent=2), encoding='utf-8')
print(json.dumps(report, ensure_ascii=False), flush=True)
