"""Small reference-preservation comparison using original adult characters."""
import concurrent.futures
import io
import json
from pathlib import Path
import sys
import time
import uuid

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from PIL import Image
from server.app.ai import settings
from server.app.manga.models import validate_config
from server.app.manga.provider import ImageAPIProvider

ROOT = Path('.tmp/model-eval')
STYLE = 'Japanese anime comic illustration, clean line art, consistent cel shading, no text, no watermark. '
PROMPTS = {
    'lin': STYLE + 'One adult woman, age 30, short straight black hair, brown eyes, red jacket over a white shirt. Front facing waist-up character reference, plain light grey background, both hands visible, neutral expression. Exactly one person.',
    'zhou': STYLE + 'One adult man, age 35, short curly auburn hair, green eyes, round black glasses, dark green jacket over a beige shirt. Front facing waist-up character reference, plain light grey background, both hands visible, neutral expression. Exactly one person.',
}
DUO = STYLE + 'Exactly two adults in a greenhouse in morning light. The person from reference image 1 stands on the LEFT of a wooden table and extends a small copper key toward the other person. The person from reference image 2 stands on the RIGHT and reaches with an open hand to accept the key. Preserve both reference faces, hair, clothing and accessories exactly. Keep the two identities distinct and do not swap them. Medium shot, both people and the handoff clearly visible. No other people, no text, no speech bubbles.'


def generate(connection, model, name, prompt, refs=None):
    path = ROOT / (name + '.png')
    cfg = settings.load_settings('profile-' + connection)
    image_config = validate_config({'provider':'openai-images','base_url':cfg['base_url'],'model':model,
                                   'width':1024,'height':1024,'timeout':180,'rpm':25,'max_concurrency':2,'reference_api':'generations'})
    start = time.monotonic()
    result = {'connection':connection,'model':model,'name':name,'references':len(refs) if isinstance(refs,list) else int(refs is not None)}
    try:
        raw = ImageAPIProvider(image_config, credential_slot='profile-' + connection).generate({'prompt':prompt}, refs, uuid.uuid4().hex)
        image = Image.open(io.BytesIO(raw)); image.load(); image.save(path)
        result.update(ok=True,width=image.width,height=image.height,path=str(path))
    except Exception as exc:
        result.update(ok=False,error=str(exc))
    result['seconds']=round(time.monotonic()-start,2)
    (ROOT/(name+'.result.json')).write_text(json.dumps(result,ensure_ascii=False,indent=2),encoding='utf-8')
    print(json.dumps(result,ensure_ascii=False),flush=True)
    return result


if __name__ == '__main__':
    ROOT.mkdir(parents=True,exist_ok=True)
    with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
        refs=list(pool.map(lambda key: json.loads((ROOT/('ref-'+key+'.result.json')).read_text(encoding='utf-8')) if (ROOT/('ref-'+key+'.png')).is_file() else generate('agnes','agnes-image-2.1-flash','ref-'+key,PROMPTS[key]), PROMPTS))
    if not all(r['ok'] for r in refs): raise SystemExit('Reference generation failed; comparison stopped.')
    references=[ROOT/'ref-lin.png', ROOT/'ref-zhou.png']
    models=['agnes-image-2.0-flash','agnes-image-2.1-flash','agnes-image-2.5-flash']
    with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
        results=list(pool.map(lambda model:generate('agnes',model,model+'-duo',DUO,references),models))
    results.append(generate('pie-image','agnes-image-2.5-flash','relay-2.5-duo',DUO,references))
    (ROOT/'image-results.json').write_text(json.dumps(refs+results,ensure_ascii=False,indent=2),encoding='utf-8')
