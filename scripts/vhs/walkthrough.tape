# Animated README walkthrough GIF: db picker through a Playground query
# execution. Run against the synthetic `pgpilot_demo` db (see
# demo_seed.sql) — generic e-commerce schema, safe to publish.
#
# For Activity to show a live query, kick off a slow background one first
# (needs to outlive the whole recording, ~30s):
#   psql -d pgpilot_demo -c "SELECT pg_sleep(90), count(*) FROM shop.orders o JOIN shop.order_items oi ON oi.order_id=o.id;" &
#
# Run: vhs scripts/vhs/walkthrough.tape
# Output: docs/screenshots/walkthrough.gif

Output "docs/screenshots/walkthrough.gif"

Require echo

Set Shell "bash"
Set FontSize 13
Set Width 1200
Set Height 700
Set Padding 0
Set WindowBar Colorful
Set Framerate 20
Set TypingSpeed 30ms

Type "./target/release/pgpilot --dbname pgpilot_demo"
Enter
Sleep 1800ms

Enter
Sleep 2500ms

Type "g"
Sleep 2500ms
Type "g"
Sleep 400ms

Type "2"
Sleep 1800ms

Type "3"
Sleep 2200ms

Type "4"
Sleep 2200ms

Type "5"
Sleep 2200ms

Type "6"
Sleep 600ms
Type "SELECT status, count(*), sum(total_cents) FROM shop.orders GROUP BY status ORDER BY 2 DESC;"
Sleep 500ms
Enter
Sleep 2500ms

Type "q"
Sleep 500ms
