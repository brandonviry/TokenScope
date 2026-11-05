import ctypes
import ctypes.wintypes as w
import json
import pathlib
import urllib.error
import urllib.request

class Credential(ctypes.Structure):
    _fields_ = [('Flags', w.DWORD), ('Type', w.DWORD), ('TargetName', w.LPWSTR),
                ('Comment', w.LPWSTR), ('LastWritten', w.FILETIME), ('CredentialBlobSize', w.DWORD),
                ('CredentialBlob', ctypes.POINTER(ctypes.c_ubyte)), ('Persist', w.DWORD),
                ('AttributeCount', w.DWORD), ('Attributes', ctypes.c_void_p),
                ('TargetAlias', w.LPWSTR), ('UserName', w.LPWSTR)]

adv = ctypes.WinDLL('Advapi32.dll', use_last_error=True)
adv.CredReadW.argtypes = [w.LPCWSTR, w.DWORD, w.DWORD, ctypes.POINTER(ctypes.POINTER(Credential))]
adv.CredReadW.restype = w.BOOL
pointer = ctypes.POINTER(Credential)()
assert adv.CredReadW('gemini:antigravity', 1, 0, ctypes.byref(pointer))
try:
    value = json.loads(ctypes.string_at(pointer.contents.CredentialBlob, pointer.contents.CredentialBlobSize))
finally:
    adv.CredFree(pointer)

token = value['token']['access_token']
print('Native token expiry:', value['token'].get('expiry'), flush=True)
payloads = {}
project = None
for operation, body in [
    ('loadCodeAssist', {'metadata': {'ideType': 'IDE_UNSPECIFIED', 'platform': 'PLATFORM_UNSPECIFIED', 'pluginType': 'GEMINI'}, 'mode': 'HEALTH_CHECK'}),
    ('fetchAvailableModels', {}),
    ('retrieveUserQuota', {}),
]:
    if operation == 'retrieveUserQuota':
        if not isinstance(project, str): continue
        body = {'project': project}
    url = f'https://daily-cloudcode-pa.googleapis.com/v1internal:{operation}'
    req = urllib.request.Request(url, data=json.dumps(body).encode(), headers={
        'Authorization': 'Bearer ' + token, 'Content-Type': 'application/json',
        'User-Agent': 'antigravity/1.4.0 TokenScope/1.1.0 windows/amd64',
    })
    try:
        with urllib.request.urlopen(req, timeout=12) as response:
            data = json.load(response)
        print(operation, 'response keys:', list(data), flush=True)
        if operation == 'loadCodeAssist':
            project = data.get('cloudaicompanionProject')
            for key in ['currentTier', 'paidTier']:
                if isinstance(data.get(key), dict):
                    print(key, {k: data[key][k] for k in ['id', 'name', 'availableCredits'] if k in data[key]}, flush=True)
            payloads[operation] = {key: data[key] for key in ['currentTier', 'paidTier'] if isinstance(data.get(key), dict)}
        elif operation == 'retrieveUserQuota':
            print('buckets:', json.dumps(data.get('buckets', []))[:3500], flush=True)
        else:
            models = data.get('models', {})
            print('model count:', len(models), flush=True)
            safe_models = {}
            for model_id, model in models.items():
                if isinstance(model, dict) and ('gemini' in model_id.lower() or model.get('quotaInfo')):
                    safe_models[model_id] = {k: model[k] for k in ['displayName', 'modelProvider', 'quotaInfo'] if k in model}
                    print(model_id, json.dumps(safe_models[model_id]), flush=True)
            payloads[operation] = {'models': safe_models, 'agentModelSorts': data.get('agentModelSorts', [])}
    except urllib.error.HTTPError as error:
        print(operation, 'HTTP', error.code, flush=True)
        try:
            failure=json.load(error)['error']
            print('error classification:',failure.get('status'), [x.get('reason') for x in failure.get('details',[])], flush=True)
            import re
            print('message:',re.sub(r'[\w.+-]+@[\w.-]+','[account]',failure.get('message',''))[:400],flush=True)
        except Exception: pass
    except Exception as error:
        print(operation, type(error).__name__, flush=True)

if payloads:
    (pathlib.Path(__file__).resolve().parent / 'antigravity-live-response.json').write_text(json.dumps(payloads, indent=2), encoding='utf-8')
