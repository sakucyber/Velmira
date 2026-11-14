"""Source chronology, semantic compression, atomic recovery and real budgets."""
import asyncio
import json
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock, patch

from server.app import db
from server.app.ai import chapter_memory as cm
from server.app.manga.models import MangaError, digest


def memory_text(ending=""):
    text = ending
    while cm.token_count(text)[0] < 110:
        text += " Ari keeps the key and waits for the next event."
    return text.strip()


class ChapterMemoryTests(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory(); self.addCleanup(tmp.cleanup)
        p = patch("server.app.db.DB_PATH", Path(tmp.name) / "app.db"); p.start(); self.addCleanup(p.stop)
        p = patch("server.app.ai.retry.RETRY_DELAY", 0); p.start(); self.addCleanup(p.stop)
        db.init_db(); self.conn = db.get_conn(); self.addCleanup(self.conn.close)
        with self.conn:
            self.conn.execute("INSERT INTO books(id,title,file_path,sha256,created_at) VALUES('b','Book','x','hash',1)")
            for chapter, text in enumerate(["Ari has black hair. Later, Ari has blue hair.", "Ari opens the door. Finally the key breaks."]):
                self.conn.execute("INSERT INTO chapters(id,book_id,idx,title,chars,paras_json) VALUES(?,?,?,?,?,?)", (str(chapter), 'b', chapter, str(chapter), len(text), json.dumps([text])))

    def row(self, chapter):
        return dict(self.conn.execute("SELECT idx,title,paras_json FROM chapters WHERE book_id='b' AND idx=?", (chapter,)).fetchone())

    def parsed(self, chapter=0, facts=None, ending=""):
        row = self.row(chapter); text = "\n".join(json.loads(row["paras_json"]))
        return cm.normalize({"memory": memory_text(ending), "facts": facts or []}, text, book_id='b', chapter=chapter, source_hash=digest(row))

    def test_semantic_model_keeps_ending_in_single_chunk(self):
        row = self.row(1); text = json.loads(row['paras_json'])[0]
        response = {"memory": memory_text("The key breaks at the end."), "facts": [{"kind":"prop","character_id":"Ari","field":"key","value":"broken","quote":"the key breaks"}]}
        provider = SimpleNamespace(complete=AsyncMock(return_value=response))
        result = asyncio.run(cm.compress(provider, book_id='b', chapter=1, title='1', pieces=[{'text':text}], parts=[{'synopsis':'Ari opens the door.'}], source_hash=digest(row)))
        self.assertIn("breaks", result['text']); self.assertEqual(result['facts'][0]['value'], 'broken')
        self.assertEqual(provider.complete.await_count, 1)
        self.assertEqual(json.loads(provider.complete.await_args.kwargs['user'])['text'], text)
        self.assertTrue(100 <= result['tokens'] <= 250)

    def test_invalid_length_or_unquoted_fact_rejected(self):
        with self.assertRaises(MangaError): cm.normalize({'memory':'too short'}, '', book_id='b',chapter=0,source_hash='s')
        with self.assertRaises(MangaError): self.parsed(facts=[{'kind':'event','field':'action','value':'invented','quote':'not in the chapter'}])

    def test_end_offset_filter_and_distinct_transformation_stages(self):
        facts = [{'kind':'state','character_id':'Ari','field':'hair_color','value':value,'quote':quote} for value,quote in [('black','black hair'),('blue','blue hair')]]
        memory = self.parsed(facts=facts)
        with self.conn: cm.save(self.conn,'b',0,digest(self.row(0)),memory)
        self.assertEqual(cm.facts_before(self.conn,'b',0), [])
        cutoff = memory['facts'][0]['end_offset'] - 1
        self.assertEqual(cm.facts_before(self.conn,'b',0,cutoff), [])
        self.assertEqual(cm.state_before(self.conn,'b',0,cutoff+1)[0]['value'], 'black')
        self.assertEqual(cm.state_before(self.conn,'b',0,999)[0]['value'], 'blue')
        self.assertEqual(len(cm.facts_before(self.conn,'b',0,999)), 2)

    def test_source_invalidation_and_ids_include_source_revision(self):
        fact={'kind':'event','character_id':'Ari','field':'event','value':'black hair','quote':'black hair'}
        old=self.parsed(facts=[fact]); old_id=old['facts'][0]['id']
        with self.conn:
            cm.save(self.conn,'b',0,digest(self.row(0)),old)
            self.conn.execute("UPDATE chapters SET title='changed' WHERE idx=0")
        self.assertEqual(cm.facts_before(self.conn,'b',1,999), [])
        self.assertNotEqual(self.parsed(facts=[fact])['facts'][0]['id'],old_id)
        self.assertEqual(cm.snapshot(self.conn,'b',1,before_offset=0)['capsules'], [])

    def test_final_serialized_budget_and_character_fact_priority(self):
        with self.conn:
            cm.save(self.conn,'b',0,digest(self.row(0)),self.parsed(facts=[{'kind':'state','character_id':'Ari','field':'hair_color','value':'blue','quote':'blue hair'}]))
        snap=cm.snapshot(self.conn,'b',1,before_offset=0,character_ids=['Ari'],max_tokens=600)
        self.assertLessEqual(cm.token_count(cm.stable_json(snap))[0],600)
        self.assertEqual(snap['used_tokens'],cm.token_count(cm.stable_json(snap))[0])
        self.assertEqual(snap['facts'][0]['character_id'],'Ari')
        self.assertNotIn(1,[item['chapter'] for item in snap['capsules']])

    def test_failed_compression_keeps_previous_capsule(self):
        with self.conn: cm.save(self.conn,'b',0,digest(self.row(0)),self.parsed())
        previous=dict(self.conn.execute('SELECT * FROM chapter_memory_capsules').fetchone())
        provider=SimpleNamespace(complete=AsyncMock(return_value={'memory':'short','facts':[]}))
        with self.assertRaises(MangaError): asyncio.run(cm.compress(provider,book_id='b',chapter=0,title='0',pieces=[{'text':'new'}],parts=[],source_hash='new'))
        self.assertEqual(dict(self.conn.execute('SELECT * FROM chapter_memory_capsules').fetchone()),previous)

    def test_offline_fallback_is_explicit_utf8_estimate(self):
        with patch.dict('sys.modules', {'tiktoken':None}):
            count,method=cm.token_count('汉字🙂')
        self.assertEqual(count,len('汉字🙂'.encode('utf-8')))
        self.assertIn('estimated:utf8-bytes',method)

    def test_transaction_failure_keeps_previous_version_and_archive(self):
        with self.conn: cm.save(self.conn,'b',0,digest(self.row(0)),self.parsed())
        old=dict(self.conn.execute('SELECT * FROM chapter_memory_capsules').fetchone())
        broken={**self.parsed(), 'facts':[{'id':'invalid'}]}
        with self.assertRaises(KeyError):
            with self.conn: cm.save(self.conn,'b',0,digest(self.row(0)),broken)
        self.assertEqual(dict(self.conn.execute('SELECT * FROM chapter_memory_capsules').fetchone()),old)
        self.assertEqual(self.conn.execute('SELECT COUNT(*) FROM chapter_memory_history').fetchone()[0],1)

    def test_exact_fact_dedup_keeps_distinct_positions(self):
        text='Ari waits. Ari waits.'
        fact={'kind':'event','character_id':'Ari','field':'waiting','value':'waiting','quote':'Ari waits.'}
        with self.assertRaises(MangaError): cm.normalize({'memory':memory_text(),'facts':[fact]},text,book_id='b',chapter=0,source_hash='s')
        out=cm.normalize({'memory':memory_text(),'facts':[{**fact,'start_offset':0},{**fact,'start_offset':11}]},text,book_id='b',chapter=0,source_hash='s')
        self.assertNotEqual(out['facts'][0]['id'],out['facts'][1]['id'])


if __name__=='__main__': unittest.main()
