"""Read-only prose/panel mapping, source validity and book/run isolation."""
import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from server.app import db
from server.app.manga.models import digest, MangaError
from server.app.manga import composition, reading_map as rm


class ReadingMapTests(unittest.TestCase):
    def setUp(self):
        temp=tempfile.TemporaryDirectory(); self.addCleanup(temp.cleanup)
        self.root=Path(temp.name)
        for target,value in [('server.app.db.DB_PATH',self.root/'app.db'),('server.app.config.DATA_DIR',self.root)]:
            p=patch(target,value);p.start();self.addCleanup(p.stop)
        db.init_db();self.conn=db.get_conn();self.addCleanup(self.conn.close)
        self.text='甲🙂乙。\n丙在门前停步。'
        with self.conn:
            for book in ['b','other']:
                self.conn.execute("INSERT INTO books(id,title,file_path,sha256,created_at) VALUES(?,?,?,?,1)",(book,book,book,book))
                self.conn.execute("INSERT INTO chapters VALUES(?,?,?,?,?,?)",(book+'-ch',book,0,'Title',len(self.text),json.dumps(self.text.split('\n'),ensure_ascii=False)))
                self.conn.execute("INSERT INTO scenes(id,book_id,chapter_id,segment_id,plan_json,created_at,updated_at) VALUES(?,?,0,'segment','{}',1,1)",(book+'-scene',book))
            self.conn.execute("INSERT INTO auto_manga(book_id,run_id,start_chapter,chapter,status,source_hash,updated_at) VALUES('b','current',0,0,'done','source',1)")
        self.asset('page.png');self.asset('panel.png')

    def asset(self,name):
        folder=self.root/'assets';folder.mkdir(exist_ok=True);(folder/name).write_bytes(b'image')
        return 'assets/'+name

    def chapter_hash(self,book='b'):
        row=self.conn.execute("SELECT title,paras_json FROM chapters WHERE book_id=? AND idx=0",(book,)).fetchone()
        return digest(dict(row))

    def board(self,ident='board',book='b'):
        plan={'source_hash':'source','material':{'chapter':0,'chapter_hash':self.chapter_hash(book)}}
        with self.conn:self.conn.execute("INSERT INTO storyboards(id,book_id,scene_id,cache_key,plan_json,created_at,updated_at) VALUES(?,?,?,?,?,1,1)",(ident,book,book+'-scene',ident,json.dumps(plan)))
        return ident

    def job(self,ident='job',book='b',span=None,board='board',**changes):
        if span is None:span={'chapter':0,'start':0,'end':4,'quote':self.text[:4]}
        snap={'source_span':span,'board_id':board,'source_hash':'source',**changes}
        with self.conn:self.conn.execute("INSERT INTO render_jobs(id,book_id,kind,status,snapshot_json,config_hash,image_path,created_at,updated_at) VALUES(?,?,'panel','approved',?,'config','assets/panel.png',1,1)",(ident,book,json.dumps(snap)))
        return ident

    def page(self,ident='page',jobs=None,run=None,book='b',span=None):
        jobs=jobs or ['job']
        spec=composition.validate({'width':640,'height':960,'panels':[{'job_id':job,**({'source_span':span} if span is not None else {})} for job in jobs]})
        sources=[{'job_id':job,**({'source_span':span} if span is not None else {})} for job in jobs]
        with self.conn:
            if run:
                self.conn.execute("INSERT INTO auto_manga_pages VALUES(?,?,?,?,?,?,?,?,?,?,?)",(ident,book,run,ident+'-scene',0,0,json.dumps(spec),json.dumps(sources),'assets/page.png','hash',1))
            else:self.conn.execute("INSERT INTO manga_pages(id,book_id,position,spec_json,sources_json,image_path,image_hash,created_at,updated_at) VALUES(?,?,0,?,?,'assets/page.png','hash',1,1)",(ident,book,json.dumps(spec),json.dumps(sources)))

    def test_current_run_isolation_and_explicit_history(self):
        self.board();self.job();self.job('past-job')
        self.page('new-page',run='current');self.page('old-page',jobs=['past-job'],run='previous')
        current=rm.reading_map(self.conn,'b')
        self.assertEqual(current['run_id'],'current')
        self.assertEqual([p['page_id'] for p in current['panels']],['new-page'])
        self.assertEqual([p['page_id'] for p in rm.reading_map(self.conn,'b',run_id='previous')['panels']],['old-page'])

    def test_snapshot_span_has_priority_and_unicode_half_open(self):
        self.board();self.job();self.page(span={'chapter':0,'start':5,'end':6,'quote':self.text[5:6]})
        value=rm.reading_map(self.conn,'b')
        self.assertEqual(value['offset_unit'],'unicode_codepoints');self.assertEqual(value['range_end'],'exclusive')
        panel=value['panels'][0]
        self.assertEqual((panel['start'],panel['end'],panel['source_quote']),(0,4,'甲🙂乙。'))
        self.assertTrue(all(0<=v<=1 for v in panel['rect']))

    def test_changed_context_invalidates_even_when_quote_unchanged(self):
        self.board();self.job();self.page()
        self.assertEqual(len(rm.reading_map(self.conn,'b')['panels']),1)
        with self.conn:self.conn.execute("UPDATE chapters SET paras_json=? WHERE book_id='b'",(json.dumps([self.text+'新增结局']),))
        self.assertEqual(rm.reading_map(self.conn,'b')['panels'],[])

    def test_missing_or_mismatched_quote_and_missing_chapter_rejected(self):
        self.board()
        spans=[{'chapter':0,'start':0,'end':4}, {'chapter':0,'start':0,'end':4,'quote':'wrong'}, {'chapter':2,'start':0,'end':1,'quote':'x'}, {'chapter':True,'start':0,'end':1,'quote':'甲'}]
        for i,span in enumerate(spans):self.job(str(i),span=span);self.page('p'+str(i),jobs=[str(i)])
        self.assertEqual(rm.reading_map(self.conn,'b')['panels'],[])

    def test_cross_book_job_and_board_cannot_supply_mapping(self):
        self.board(book='other');self.job(book='other');self.page()
        self.assertEqual(rm.reading_map(self.conn,'b')['panels'],[])
        self.job('local',book='b');self.page('local-page',jobs=['local'])
        self.assertEqual(rm.reading_map(self.conn,'b')['panels'],[])
        with self.assertRaises(MangaError):rm.reading_map(self.conn,'missing')

    def test_legacy_without_mapping_or_fingerprint_skipped(self):
        self.board();self.job(source_span=None);self.page()
        self.assertEqual(rm.reading_map(self.conn,'b')['panels'],[])
        with self.conn:self.conn.execute("UPDATE render_jobs SET snapshot_json=?",(json.dumps({'source_span':{'chapter':0,'start':0,'end':4,'quote':self.text[:4]}}),))
        self.assertEqual(rm.reading_map(self.conn,'b')['panels'],[])

    def test_manual_and_automatic_duplicate_job_is_emitted_once(self):
        self.board();self.job();self.page();self.page('draft',run='current')
        panels=rm.reading_map(self.conn,'b')['panels']
        self.assertEqual(len(panels),1);self.assertFalse(panels[0]['draft'])

    def test_missing_assets_stale_jobs_and_title_changes_skip(self):
        self.board();self.job();self.page()
        with self.conn:self.conn.execute("UPDATE chapters SET title='changed' WHERE book_id='b'")
        self.assertEqual(rm.reading_map(self.conn,'b')['panels'],[])
        with self.conn:self.conn.execute("UPDATE chapters SET title='Title' WHERE book_id='b'")
        (self.root/'assets'/'panel.png').unlink()
        self.assertEqual(rm.reading_map(self.conn,'b')['panels'],[])

    def test_shared_board_source_is_read_once_per_map(self):
        self.board();self.job();self.job('second');self.page(jobs=['job','second'])
        statements=[];self.conn.set_trace_callback(statements.append)
        try:self.assertEqual(len(rm.reading_map(self.conn,'b')['panels']),2)
        finally:self.conn.set_trace_callback(None)
        self.assertEqual(len([sql for sql in statements if 'SELECT plan_json FROM storyboards' in sql]),1)


if __name__=='__main__':unittest.main()
