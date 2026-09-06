# Full product tour: every pgpilot screen, db picker through a Playground
# query execution. Run against the synthetic `pgpilot_demo` db (see
# demo_seed.sql) — generic e-commerce schema, safe to publish.
#
# For Activity/Overview to show a live query, kick off a slow background
# one first (needs to outlive the whole recording, ~30-40s):
#   psql -d pgpilot_demo -c "SELECT pg_sleep(180), count(*) FROM shop.orders o JOIN shop.order_items oi ON oi.order_id=o.id;" &
#
# Note: always leave a Sleep after a Screenshot before the next Type/key —
# Screenshot's capture is async and races the next keystroke otherwise
# (bit us once: no gap left "g" pressed before the prior screenshot fired,
# so 02-overview.png actually captured the diagnosis modal from step 03).
#
# Run: vhs scripts/vhs/tour.tape
# Output: docs/screenshots/01-db-picker.png .. 08-playground.png

Output "docs/screenshots/01-db-picker.png"

Require echo

Set Shell "bash"
Set FontSize 14
Set Width 1400
Set Height 900
Set Padding 0
Set WindowBar Colorful

Type "./target/release/pgpilot --dbname pgpilot_demo"
Enter
Sleep 1500ms
Screenshot "docs/screenshots/01-db-picker.png"
Sleep 500ms

Enter
Sleep 3000ms
Screenshot "docs/screenshots/02-overview.png"
Sleep 500ms

Type "g"
Sleep 3000ms
Screenshot "docs/screenshots/03-diagnosis-modal.png"
Sleep 500ms
Type "g"
Sleep 300ms

Type "2"
Sleep 1500ms
Screenshot "docs/screenshots/04-queries.png"
Sleep 500ms

Type "3"
Sleep 1500ms
Screenshot "docs/screenshots/05-activity.png"
Sleep 500ms

Type "4"
Sleep 1500ms
Screenshot "docs/screenshots/06-tables-indexes.png"
Sleep 500ms

Type "5"
Sleep 1500ms
Screenshot "docs/screenshots/07-triggers.png"
Sleep 500ms

Type "6"
Sleep 800ms
Type "SELECT status, count(*), sum(total_cents) FROM shop.orders GROUP BY status ORDER BY 2 DESC;"
Sleep 500ms
Enter
Sleep 2000ms
Screenshot "docs/screenshots/08-playground.png"
Sleep 500ms

Type "q"
Sleep 500ms
