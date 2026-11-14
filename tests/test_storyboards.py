import io
import asyncio
import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
import threading
import time
from unittest.mock import AsyncMock, patch

from fastapi.testclient import TestClient
from PIL import Image
from server.app import db
from server.app.ai.models import render_identity, identity_hash
from server.app.main import create_app
from server.app.manga import storyboard, queue
from manga_fixture import seed

TEXT = '林澄站在桌左侧。周遥站在右侧。林澄把铜钥匙递给周遥。两人离开后，桌上只剩一片树叶。'


def panel(kind, ids):
    groups = {
        'solo': (0, 1, '林澄站在桌左侧。周遥站在右侧。'),
        'duo': (2, 2, '林澄把铜钥匙递给周遥。'),
        'environment': (3, 3, '两人离开后，桌上只剩一片树叶。'),
    }
    first, last, quote = groups[kind]
    return {'kind':kind, 'shot':'wide', 'subjects':[{'character_id':cid,'position':'center' if len(ids)==1 else 'left' if i==0 else 'right','pose':'stand beside the table'} for i,cid in enumerate(ids)],
            'action':'hand over a copper key' if kind=='duo' else 'a quiet moment', 'environment':'wooden table', 'lighting':'', 'emotion':'',
            'caption':'', 'dialogue':'', 'speaker':'', 'source_quote':quote,
            'start_sentence':first, 'end_sentence':last}


class StoryboardTests(unittest.TestCase):
    def setUp(self):
        tmp=tempfile.TemporaryDirectory();self.addCleanup(tmp.cleanup);self.root=Path(tmp.name)
        for target,value in [('server.app.db.DB_PATH',self.root/'app.db'),('server.app.config.DATA_DIR',self.root),('server.app.ai.retry.RETRY_DELAY',0)]:
            p=patch(target,value);p.start();self.addCleanup(p.stop)
        db.init_db();seed();queue._stop.clear()
        self.client=TestClient(create_app());self.addCleanup(self.client.close)
        self.base='/api/books/m3-book/manga'
        self.client.patch(self.base+'/config',json={'provider':'mock','width':256,'height':256})
        self.client.patch('/api/ai/settings',json={'base_url':'https://text.example/v1','model':'fixture','enabled':True,'auto_analyze':False})
        identity=render_identity({'outfit':'green jacket'},'周遥')
        self.sql("UPDATE characters SET name='林澄' WHERE id='m3-char'")
        self.sql("INSERT INTO characters(id,book_id,name,card_json,frozen,identity_block,identity_hash,created_at,updated_at) VALUES('second','m3-book','周遥','{}',1,?,?,1,1)",(identity,identity_hash(identity)))
        self.sql('UPDATE chapters SET paras_json=?',(json.dumps([TEXT]),))
        self.sql('UPDATE segments SET text=?',(TEXT,))
        self.sql('UPDATE scenes SET plan_json=?',(json.dumps({'summary':'交接钥匙','character_ids':['m3-char','second']}),))
        self.panels=[panel('solo',['m3-char']),panel('duo',['m3-char','second']),panel('environment',[])]
        self.provider=SimpleNamespace(complete=AsyncMock(return_value={'panels':self.panels}))
        p=patch.object(storyboard,'make_provider',return_value=self.provider);p.start();self.addCleanup(p.stop)

    def sql(self,query,args=()):
        conn=db.get_conn()
        try:
            with conn:return [dict(r) for r in conn.execute(query,args)]
        finally:conn.close()

    def create(self):
        response=self.client.post(self.base+'/storyboards',json={'scene_id':'m3-scene'})
        self.assertEqual(response.status_code,200,response.text)
        return response.json()['storyboard']

    def refs(self):
        self.client.get(self.base+'/forms')
        for fid in ['form_m3-char','form_second']:
            image=io.BytesIO();Image.new('RGB',(64,64),'white').save(image,'PNG')
            response=self.client.post(self.base+'/forms/'+fid+'/refs',content=image.getvalue())
            self.assertEqual(response.status_code,201,response.text)
            self.client.patch(self.base+'/refs/'+response.json()['ref']['id'],json={'accepted':True})

    def render(self,board,indices=None):
        return self.client.post(self.base+'/storyboards/'+board['id']+'/render',json={'revision':board['revision'],'panels':indices or [0,1,2]})

    def test_one_call_cache_edit_conflict_and_no_implicit_image_cost(self):
        board=self.create();again=self.create()
        self.assertEqual(board['id'],again['id']);self.assertEqual(self.provider.complete.await_count,1)
        self.assertEqual(self.sql('SELECT * FROM render_jobs'),[])
        edits=json.loads(json.dumps(self.panels));edits[1]['caption']='铜钥匙被递向右侧。'
        url=self.base+'/storyboards/'+board['id']
        response=self.client.patch(url,json={'revision':1,'panels':edits})
        self.assertEqual(response.status_code,200,response.text)
        self.assertEqual(response.json()['storyboard']['revision'],2)
        self.assertEqual(self.client.patch(url,json={'revision':1,'panels':edits}).status_code,409)
        self.assertEqual(self.render(board).status_code,409)

    def test_both_references_required_atomic_enqueue_and_review_semantics(self):
        board=self.create()
        self.assertEqual(self.render(board).status_code,409)
        self.assertEqual(self.sql('SELECT * FROM render_jobs'),[])
        self.refs();response=self.render(board)
        self.assertEqual(response.status_code,200,response.text)
        jobs=response.json()['jobs']
        self.assertEqual(len(jobs[1]['snapshot']['cast']),2)
        self.assertIn('reference image 2',jobs[1]['snapshot']['prompt'])
        self.assertIn('no people',jobs[2]['snapshot']['prompt'])
        self.assertNotIn('face clearly visible',jobs[2]['snapshot']['prompt'])
        self.assertEqual([j['id'] for j in self.render(board).json()['jobs']],[j['id'] for j in jobs])
        for job in jobs:
            self.sql("UPDATE render_jobs SET status='running' WHERE id=?",(job['id'],));queue.process(job['id'])
        self.assertTrue(all(r['status']=='needs_review' for r in self.sql('SELECT status FROM render_jobs')))
        for job in jobs[1:]:
            self.assertEqual(self.client.post(self.base+'/jobs/'+job['id']+'/reference',json={}).status_code,409)
            reroll=self.client.post(self.base+'/jobs/'+job['id']+'/reroll',json={})
            self.assertEqual(reroll.status_code,200,reroll.text)
            self.assertNotEqual(reroll.json()['job']['snapshot']['seed'],job['snapshot']['seed'])

    def test_reference_preview_exposes_active_form_and_validated_selection(self):
        board = self.create(); self.refs()
        loaded = self.client.get(self.base + '/storyboards/' + board['id']).json()['storyboard']
        self.assertEqual(len(loaded['reference_preview']), 3)
        solo = loaded['reference_preview'][0]['subjects'][0]
        self.assertEqual(solo['form']['id'], 'form_m3-char')
        self.assertEqual(len(solo['references']), 1)
        selected = solo['references'][0]['id']
        edited = json.loads(json.dumps(self.panels)); edited[0]['subjects'][0]['reference_id'] = selected
        saved = self.client.patch(self.base + '/storyboards/' + board['id'], json={'revision': loaded['revision'], 'panels': edited})
        self.assertEqual(saved.status_code, 200, saved.text)
        self.assertEqual(saved.json()['storyboard']['panels'][0]['subjects'][0]['reference_id'], selected)
        foreign = json.loads(json.dumps(edited)); foreign[0]['subjects'][0]['reference_id'] = 'missing-ref'
        self.assertEqual(self.client.patch(self.base + '/storyboards/' + board['id'], json={'revision': 2, 'panels': foreign}).status_code, 409)
        jobs = self.render(saved.json()['storyboard'], [0]).json()['jobs']
        self.assertEqual(jobs[0]['snapshot']['cast'][0]['reference']['id'], selected)

    def test_second_character_revocation_and_changed_source_invalidate_results(self):
        board=self.create();self.refs();jobs=self.render(board).json()['jobs']
        second=self.sql("SELECT id FROM form_refs WHERE form_id='form_second'")[0]['id']
        self.client.patch(self.base+'/refs/'+second,json={'accepted':False})
        duo=self.client.get(self.base+'/jobs/'+jobs[1]['id']).json()['job']
        self.assertEqual(duo['status'],'stale')
        self.sql("UPDATE chapters SET title='changed'")
        self.assertEqual(self.client.get(self.base+'/storyboards/'+board['id']).status_code,409)

    def test_malformed_fields_foreign_cast_and_unquoted_dialogue_rejected(self):
        board=self.create();url=self.base+'/storyboards/'+board['id']
        changes=[{'kind':[]},{'shot':[]},{'source_quote':'不存在的原文'},{'subjects':[{'character_id':'foreign','position':'left','pose':'stand'}]}, {'dialogue':'编造对白','speaker':'林澄'}]
        for change in changes:
            response=self.client.patch(url,json={'revision':1,'panels':[{**self.panels[0],**change}]})
            self.assertIn(response.status_code,[400,502],response.text)
        self.sql("INSERT INTO books(id,title,file_path,sha256,created_at) VALUES('other','Other','other','other',1)")
        self.assertEqual(self.client.get(url.replace('m3-book','other')).status_code,404)

    def test_edit_invalidates_only_changed_panel_and_comfy_rejects_duo(self):
        board=self.create();self.refs();jobs=self.render(board).json()['jobs']
        changed=json.loads(json.dumps(self.panels));changed[1]['action']='receive the copper key'
        self.client.patch(self.base+'/storyboards/'+board['id'],json={'revision':1,'panels':changed})
        self.assertEqual(self.client.get(self.base+'/jobs/'+jobs[1]['id']).json()['job']['status'],'stale')
        self.assertEqual(self.client.get(self.base+'/jobs/'+jobs[0]['id']).json()['job']['status'],'queued')
        self.client.patch(self.base+'/config',json={'provider':'comfyui'})
        board=self.client.get(self.base+'/storyboards/'+board['id']).json()['storyboard']
        self.assertEqual(self.render(board,[1]).status_code,409)

    def test_cloud_queue_runs_two_jobs_at_once_and_finishes_each_once(self):
        self.client.patch(self.base+'/config',json={'provider':'openai-images','base_url':'https://images.example/v1','model':'fixture','max_concurrency':2})
        board=self.create();self.refs();jobs=self.render(board).json()['jobs']
        active=0;peak=0;finished=[];lock=threading.Lock()
        def process(job_id):
            nonlocal active,peak
            with lock:active+=1;peak=max(peak,active)
            time.sleep(.05)
            self.sql("UPDATE render_jobs SET status='needs_review' WHERE id=?",(job_id,))
            with lock:
                active-=1;finished.append(job_id)
                if len(finished)==3:queue._stop.set()
        async def run():
            with patch.object(queue,'process',side_effect=process):
                await asyncio.wait_for(queue.loop(),5)
        try:asyncio.run(run())
        finally:queue._stop.clear()
        self.assertEqual(peak,2)
        self.assertCountEqual(finished,[j['id'] for j in jobs])


if __name__=='__main__':unittest.main()
