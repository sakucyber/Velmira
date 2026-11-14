"""A multi-page scene resumes without repeating pagination, panels or pages."""
import json
from types import SimpleNamespace
import unittest
from unittest.mock import AsyncMock, patch

import test_automatic_manga as fixture
from server.app import db
from server.app.manga import pagination


TEXT = "他进入公园。他走到长椅旁。他打开背包。他取出地图。他查看小路。他收好地图。他沿小路离开。"


class AutomaticPaginationTests(unittest.TestCase):
    # Reuse the disposable application fixture without inheriting its test_* methods.
    setUp = fixture.AutomaticTests.setUp
    sql = fixture.AutomaticTests.sql
    call = fixture.AutomaticTests.call
    advance = fixture.AutomaticTests.advance
    finish = fixture.AutomaticTests.finish

    def test_two_and_five_panel_pages_resume_without_repeating_model_or_outputs(self):
        self.sql("UPDATE chapters SET paras_json=?,chars=? WHERE id='ch2'", (json.dumps([TEXT]), len(TEXT)))
        self.sql("UPDATE segments SET text=? WHERE id='seg2'", (TEXT,))
        planned = {"pages": [
            {"start_sentence": 0, "end_sentence": 1, "panels": 2, "layout": "2x1"},
            {"start_sentence": 2, "end_sentence": 6, "panels": 5, "layout": "1+2+2"},
        ]}
        page_provider = SimpleNamespace(complete=AsyncMock(return_value=planned))
        seen = []
        async def story(**kwargs):
            material = json.loads(kwargs["user"])
            seen.append(material)
            return {"panels": [
                {"kind": "solo", "subjects": [{"character_id": "m3-char", "position": "center", "pose": "walk beside the park path"}],
                 "shot": "medium", "action": "walk beside the park path", "environment": "a quiet park",
                 "lighting": "daylight", "emotion": "", "caption": sentence["text"], "dialogue": "", "speaker": "",
                 "start_sentence": index, "end_sentence": index, "source_quote": sentence["text"]}
                for index, sentence in enumerate(material["sentences"])]}
        self.story_provider.complete.side_effect = story
        with patch.object(pagination, "make_provider", return_value=page_provider):
            started = self.call(chapter=1)
            for _ in range(20):
                self.advance()
                first = self.sql("SELECT * FROM auto_manga_pages ORDER BY ordinal")
                if first: break
            self.assertEqual(len(first), 1)
            self.assertEqual(len(json.loads(first[0]["spec_json"])["panels"]), 2)
            first_identity = (first[0]["id"], first[0]["image_path"], first[0]["image_hash"])
            state = json.loads(self.sql("SELECT state_json FROM auto_manga")[0]["state_json"])
            self.assertEqual((state["page_cursor"], state.get("scene", 0)), (1, 0))
            self.assertEqual(len(state["page_ranges"]), 2)
            self.assertEqual(page_provider.complete.await_count, 1)

            self.call(False)
            before = self.sql("SELECT id FROM render_jobs ORDER BY id")
            db.init_db()
            self.advance(3)
            self.assertEqual(self.sql("SELECT id FROM render_jobs ORDER BY id"), before)
            resumed = self.call(chapter=0)
            self.assertEqual((resumed["run_id"], resumed["start_chapter"]), (started["run_id"], 1))
            self.advance()  # Plan the second page from the durable pagination result.
            state = json.loads(self.sql("SELECT state_json FROM auto_manga")[0]["state_json"])
            self.assertEqual((state["page_cursor"], len(state["story"])), (1, 5))
            self.assertEqual(self.story_provider.complete.await_count, 2)
            self.assertTrue(seen[1]["previous_panels"])

            self.call(False)
            db.init_db()
            self.advance(3)
            self.assertEqual(page_provider.complete.await_count, 1)
            self.assertEqual(self.story_provider.complete.await_count, 2)
            self.assertEqual(self.call(chapter=0)["run_id"], started["run_id"])
            result = self.finish()
            self.assertEqual((result["pages"], result["completed_chapters"]), (2, 1))
            self.advance(3)

        rows = self.sql("SELECT * FROM auto_manga_pages ORDER BY ordinal")
        self.assertEqual(len(rows), 2)
        self.assertEqual((rows[0]["id"], rows[0]["image_path"], rows[0]["image_hash"]), first_identity)
        specs = [json.loads(row["spec_json"]) for row in rows]
        self.assertEqual([len(spec["panels"]) for spec in specs], [2, 5])
        self.assertEqual([spec["layout"] for spec in specs], ["2x1", "1+2+2"])
        self.assertEqual([row["ordinal"] for row in rows], [0, 1])
        self.assertEqual(len({row["scene_id"] for row in rows}), 2)
        panels = [panel for spec in specs for panel in spec["panels"]]
        self.assertEqual(len({panel["job_id"] for panel in panels}), 7)
        self.assertEqual("".join(panel["source_span"]["quote"] for panel in panels), TEXT)
        cursor = 0
        for panel in panels:
            span = panel["source_span"]
            self.assertEqual((span["chapter"], span["start"]), (1, cursor))
            self.assertEqual(TEXT[span["start"]:span["end"]], span["quote"])
            cursor = span["end"]
        self.assertEqual(cursor, len(TEXT))
        jobs = self.sql("SELECT kind,COUNT(*) AS n FROM render_jobs GROUP BY kind")
        self.assertEqual({row["kind"]: row["n"] for row in jobs}, {"reference": 1, "panel": 7})
        self.assertEqual(page_provider.complete.await_count, 1)
        self.assertEqual(self.story_provider.complete.await_count, 2)
        self.assertEqual(len(self.sql("SELECT id FROM storyboards")), 2)


if __name__ == "__main__": unittest.main()
