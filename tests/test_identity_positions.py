"""Same-paragraph transformations resolve at the actual source position."""
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from server.app import db
from server.app.ai import chapter_memory as cm
from server.app.manga import automatic as auto
from server.app.manga.models import digest, resolve_form
from manga_fixture import seed


class IdentityPositionTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory(); self.addCleanup(temp.cleanup)
        mocked = patch("server.app.db.DB_PATH", Path(temp.name) / "app.db")
        mocked.start(); self.addCleanup(mocked.stop)
        db.init_db(); seed()
        self.conn = db.get_conn(); self.addCleanup(self.conn.close)
        self.text = "她穿着红衣。她换上蓝衣。她又换回红衣。"
        with self.conn:
            self.conn.execute("UPDATE characters SET frozen=0,identity_block=NULL,identity_hash=NULL,card_json=?",
                              (json.dumps({"evidence": [{"field":"outfit", "value":"red jacket", "ch":0,"quote":"她穿着红衣。"}]}),))
            self.conn.execute("UPDATE chapters SET paras_json=?", (json.dumps([self.text]),))
            self.conn.execute("UPDATE segments SET text=?", (self.text,))
            row = dict(self.conn.execute("SELECT idx,title,paras_json FROM chapters").fetchone())
            facts = []
            for index, (quote, outfit) in enumerate([("她穿着红衣。", "red jacket"), ("她换上蓝衣。", "blue jacket"), ("她又换回红衣。", "red jacket")]):
                start = self.text.index(quote)
                facts.append({"id": "f" + str(index), "character_id":"m3-char", "kind":"state", "field":"outfit", "value":outfit, "quote":quote,
                              "start_offset":start,"end_offset":start + len(quote)})
            cm.save(self.conn, "m3-book", 0, digest(row), {"text":"fixture", "tokens":100, "token_method":"fixture", "status":"ok", "version":2, "facts":facts})
        self.ends = [self.text.index(q) + len(q) for q in ("她穿着红衣。", "她换上蓝衣。", "她又换回红衣。")]

    def choose(self, offset):
        with self.conn:
            fid = auto.choose_form(self.conn,"m3-book","m3-char",0,0,source_offset=offset)
        return dict(self.conn.execute("SELECT * FROM character_forms WHERE id=?",(fid,)).fetchone())

    def test_change_stays_after_complete_evidence_and_return_reuses_identity(self):
        before = self.choose(self.ends[0])
        partial = self.choose(self.ends[1] - 1)
        changed = self.choose(self.ends[1])
        returned = self.choose(self.ends[2])
        self.assertEqual(before["id"], partial["id"])
        self.assertIn("red jacket", before["identity_block"])
        self.assertIn("blue jacket", changed["identity_block"])
        self.assertEqual(returned["id"], before["id"])
        self.assertNotEqual(changed["id"], before["id"])
        forms = [dict(r) for r in self.conn.execute("SELECT * FROM character_forms")]
        self.assertEqual(resolve_form(forms,0,0,source_offset=self.ends[2])["id"],before["id"])
        self.assertEqual(resolve_form(forms,0,0,source_offset=self.ends[1])["id"],changed["id"])
        self.assertEqual(resolve_form(forms,0,0,source_offset=self.ends[0])["id"],before["id"])
        card=json.loads(returned["card_json"])
        self.assertEqual(card["source_anchor"]["offset"],self.ends[0])
        self.assertIn(self.ends[2],[anchor["offset"] for anchor in card["source_anchors"]])
        self.assertEqual(before["identity_hash"],returned["identity_hash"])
        self.assertEqual(before["version"],returned["version"])

    def test_requesting_earlier_position_after_late_generation_excludes_future_form(self):
        later = self.choose(self.ends[1])
        early = self.choose(self.ends[0])
        forms = [dict(r) for r in self.conn.execute("SELECT * FROM character_forms")]
        self.assertEqual(resolve_form(forms,0,0,source_offset=self.ends[0])["id"], early["id"])
        self.assertNotEqual(later["id"],early["id"])

    def test_reused_form_occurrence_does_not_leak_backward_and_is_idempotent(self):
        red=self.choose(self.ends[0]);blue=self.choose(self.ends[1]);returned=self.choose(self.ends[2])
        again=self.choose(self.ends[2])
        self.assertEqual(returned["id"],again["id"])
        occurrences=json.loads(again["card_json"])["source_anchors"]
        self.assertEqual(len([a for a in occurrences if a["offset"]==self.ends[2]]),1)
        # Generate late, then navigate back into and before the blue period.
        self.assertEqual(self.choose(self.ends[1])["id"],blue["id"])
        self.assertEqual(self.choose(self.ends[0])["id"],red["id"])
        forms=[dict(r) for r in self.conn.execute("SELECT * FROM character_forms")]
        for offset,expected in [(self.ends[2],red["id"]),(self.ends[1],blue["id"]),(self.ends[0],red["id"]),(self.ends[1]-1,red["id"])]:
            self.assertEqual(resolve_form(forms,0,0,source_offset=offset)["id"],expected)

    def test_unlocated_same_chapter_evidence_does_not_apply_early(self):
        with self.conn:
            self.conn.execute("UPDATE characters SET card_json=?", (json.dumps({"evidence":[{"ch":0,"field":"hair_color","value":"silver"}]}),))
        self.assertNotIn("silver",self.choose(self.ends[0])["identity_block"])


if __name__ == "__main__": unittest.main()
