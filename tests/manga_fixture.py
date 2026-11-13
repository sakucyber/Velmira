"""Neutral M3 fixture. Contains no imported book content or model weights."""
import json
from server.app import db
from server.app.ai.models import render_identity, identity_hash


def seed():
    db.init_db()
    card = {"age": "30", "gender": "woman", "hair_style": "short straight", "hair_color": "black", "eye_color": "brown", "skin": "fair", "outfit": "red jacket over white shirt"}
    identity = render_identity(card, "Reference adult")
    conn = db.get_conn()
    with conn:
        conn.execute("INSERT OR IGNORE INTO books(id,title,file_path,sha256,created_at) VALUES('m3-book','M3 neutral sample','sample.txt','m3-sample',1)")
        conn.execute("INSERT OR IGNORE INTO characters(id,book_id,name,card_json,frozen,identity_block,identity_hash,created_at,updated_at) VALUES('m3-char','m3-book','Reference adult',?,1,?,?,1,1)", (json.dumps(card), identity, identity_hash(identity)))
        conn.execute("INSERT OR IGNORE INTO chapters(id,book_id,idx,title,chars,paras_json) VALUES('m3-ch','m3-book',0,'Sample',100,'[\"Neutral sample\"]')")
        conn.execute("INSERT OR IGNORE INTO segments(id,book_id,chapter,idx,strategy,params_hash,start_para,end_para,text,features_json,score) VALUES('m3-seg','m3-book',0,0,'chapter','m3',0,9,'Neutral sample','{}',1)")
        plan = {"summary": "one adult standing in a quiet park", "location": "park", "time_of_day": "daylight", "characters": [{"name": "Reference adult"}], "character_ids": ["m3-char"], "key_props": []}
        conn.execute("INSERT OR IGNORE INTO scenes(id,book_id,chapter_id,segment_id,plan_json,created_at,updated_at) VALUES('m3-scene','m3-book',0,'m3-seg',?,1,1)", (json.dumps(plan),))
    conn.close()
