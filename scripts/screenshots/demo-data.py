"""Fill a fresh Homestock database with a demo household for the documentation screenshots.

Usage: python3 -I demo-data.py PATH/TO/homestock.sqlite3

The database must already have its admin, Alex, created through the setup screen; the other
members reuse Alex's password hash. Writes go straight to SQLite, so the search index, which
the app normally maintains itself, is rebuilt here too.
"""

import sqlite3
import sys
import time

db = sqlite3.connect(sys.argv[1])
now = int(time.time())

admin_hash = db.execute("SELECT password_hash FROM user WHERE name = 'Alex'").fetchone()[0]
for name in ["Sam", "Jordan", "Riley"]:
    db.execute("INSERT INTO user(name, role, password_hash) VALUES (?, 'member', ?)", (name, admin_hash))
user_id = {name: uid for uid, name in db.execute("SELECT id, name FROM user")}


def location(name, kind, parent=None):
    return db.execute(
        "INSERT INTO location(name, kind, parent_id) VALUES (?, ?, ?)", (name, kind, parent)
    ).lastrowid


garage = location("Garage", "Room")
kitchen = location("Kitchen", "Room")
laundry = location("Laundry", "Room")
hall = location("Hall cupboard", "Cupboard")
office = location("Office", "Room")
shelving = location("Steel shelving", "Shelf", garage)
tool_box = location("Tool box", "Box", shelving)
camping = location("Camping tub", "Bin", shelving)
christmas = location("Christmas decorations", "Box", shelving)
electrical = location("Electrical", "Drawers", garage)
pantry = location("Pantry", "Cupboard", kitchen)
baking = location("Baking tub", "Bin", pantry)
under_sink = location("Under the sink", "Cupboard", kitchen)
batteries = location("Battery drawer", "Drawers", hall)
first_aid = location("First aid box", "Box", hall)
desk = location("Desk drawers", "Drawers", office)

tag_id = {}
item_id = {}


def item(name, where, qty, unit="pcs", min_qty=0, tags=(), notes=None):
    iid = db.execute(
        "INSERT INTO item(name, notes, location_id, qty, unit, min_qty, created_at, updated_at)"
        " VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        (name, notes, where, qty, unit, min_qty, now, now),
    ).lastrowid
    for tag in tags:
        if tag not in tag_id:
            tag_id[tag] = db.execute("INSERT INTO tag(name) VALUES (?)", (tag,)).lastrowid
        db.execute("INSERT INTO item_tag VALUES (?, ?)", (iid, tag_id[tag]))
    item_id[name] = iid


item("Cordless drill", tool_box, 1, tags=("tools",), notes="18V, two batteries in the case")
item("Socket set", tool_box, 1, tags=("tools",))
item("Stud finder", tool_box, 1, tags=("tools",))
item("Tape measure", tool_box, 3, tags=("tools",))
item("Wood screws 40 mm", tool_box, 1, "box", 1, ("hardware",))
item("4-person tent", camping, 1, tags=("camping",))
item("Camp stove", camping, 1, tags=("camping",))
item("Gas canisters", camping, 2, min_qty=2, tags=("camping",))
item("Headlamp", camping, 4, tags=("camping", "lighting"))
item("Fairy lights", christmas, 6, "strings", tags=("lighting", "seasonal"))
item("Tree stand", christmas, 1, tags=("seasonal",))
item("Extension lead 5 m", electrical, 3, tags=("electrical",))
item("USB-C cables", electrical, 7, tags=("electrical",))
item("Plain flour", baking, 2, "kg", 1, ("food",))
item("Baking paper", baking, 1, "rolls", 1, ("food",))
item("Vanilla extract", baking, 0, "bottles", 1, ("food",))
item("Rice", pantry, 3, "kg", 2, ("food",))
item("Olive oil", pantry, 1, "bottles", 2, ("food",))
item("Dishwasher tablets", under_sink, 12, min_qty=20, tags=("cleaning",))
item("Sponges", under_sink, 4, min_qty=3, tags=("cleaning",))
item("AA batteries", batteries, 14, min_qty=8, tags=("electrical",))
item("AAA batteries", batteries, 3, min_qty=8, tags=("electrical",))
item("CR2032 coin cells", batteries, 5, tags=("electrical",))
item("Plasters", first_aid, 1, "box", 1, ("first aid",))
item("Paracetamol", first_aid, 2, "packs", 1, ("first aid",))
item("Printer paper", desk, 2, "reams", 1)
item("Laundry powder", laundry, 1, "boxes", 1, ("cleaning",))

for name, qty, who, days_ago in [
    ("Cordless drill", 1, "Sam", 2),
    ("Headlamp", 2, "Jordan", 5),
    ("AA batteries", 4, "Riley", 1),
    ("4-person tent", 1, "Jordan", 5),
]:
    db.execute(
        "INSERT INTO checkout(item_id, qty, user_id, checked_out_at) VALUES (?, ?, ?, ?)",
        (item_id[name], qty, user_id[who], now - days_ago * 86400),
    )

# The shopping list: some entries restock an item, some are free text, one is already bought.
for name, qty in [("Vanilla extract", 1), ("AAA batteries", 2), ("Dishwasher tablets", 1), ("Bin bags", 1), ("Milk", 2)]:
    db.execute("INSERT INTO shop(name, item_id, qty) VALUES (?, ?, ?)", (name, item_id.get(name), qty))
db.execute("INSERT INTO shop(name, qty, bought_at) VALUES ('Olive oil', 1, ?)", (now - 3600,))


# The search index, as repo::items::reindex builds it: name, notes, tags and location path.
def path(loc):
    names = []
    while loc:
        name, loc = db.execute("SELECT name, parent_id FROM location WHERE id = ?", (loc,)).fetchone()
        names.append(name)
    return " ".join(reversed(names))


for iid, loc in db.execute("SELECT id, location_id FROM item").fetchall():
    db.execute(
        "INSERT INTO item_fts(rowid, name, notes, tags, path)"
        " SELECT i.id, i.name, COALESCE(i.notes, ''),"
        "  COALESCE((SELECT group_concat(t.name, ' ') FROM item_tag it JOIN tag t ON t.id = it.tag_id"
        "            WHERE it.item_id = i.id), ''), ?"
        " FROM item i WHERE i.id = ?",
        (path(loc), iid),
    )

db.commit()
print(f"demo household: {len(user_id)} people, {len(item_id)} items")
