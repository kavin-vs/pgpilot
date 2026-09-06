-- Synthetic demo data for pgpilot screenshots. Generic e-commerce shape,
-- no real/proprietary schema. Safe to publish screenshots taken against it.

CREATE SCHEMA IF NOT EXISTS shop;
CREATE SCHEMA IF NOT EXISTS billing;
CREATE SCHEMA IF NOT EXISTS analytics;

CREATE TABLE shop.users (
    id BIGSERIAL PRIMARY KEY,
    email TEXT NOT NULL UNIQUE,
    full_name TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE shop.products (
    id BIGSERIAL PRIMARY KEY,
    sku TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    price_cents INT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE shop.orders (
    id BIGSERIAL PRIMARY KEY,
    user_id BIGINT NOT NULL REFERENCES shop.users(id),
    status TEXT NOT NULL DEFAULT 'pending',
    total_cents INT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
-- deliberately no index on orders.user_id: missing-index-candidate demo

CREATE TABLE shop.order_items (
    id BIGSERIAL PRIMARY KEY,
    order_id BIGINT NOT NULL REFERENCES shop.orders(id),
    product_id BIGINT NOT NULL REFERENCES shop.products(id),
    quantity INT NOT NULL,
    unit_price_cents INT NOT NULL
);
CREATE INDEX order_items_order_id_idx ON shop.order_items(order_id);
CREATE INDEX order_items_product_id_idx ON shop.order_items(product_id);

CREATE TABLE billing.invoices (
    id BIGSERIAL PRIMARY KEY,
    order_id BIGINT NOT NULL REFERENCES shop.orders(id),
    amount_cents INT NOT NULL,
    paid_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX invoices_order_id_idx ON billing.invoices(order_id);
CREATE INDEX invoices_paid_at_idx ON billing.invoices(paid_at); -- will end up unused

CREATE TABLE analytics.events (
    id BIGSERIAL PRIMARY KEY,
    user_id BIGINT REFERENCES shop.users(id),
    event_type TEXT NOT NULL,
    payload JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX events_user_id_idx ON analytics.events(user_id);

-- Seed data
INSERT INTO shop.users (email, full_name)
SELECT 'user' || g || '@example.com', 'Demo User ' || g
FROM generate_series(1, 5000) g;

INSERT INTO shop.products (sku, name, price_cents)
SELECT 'SKU-' || g, 'Product ' || g, (random() * 9000 + 100)::int
FROM generate_series(1, 800) g;

INSERT INTO shop.orders (user_id, status, total_cents, created_at)
SELECT (random() * 4999 + 1)::bigint,
       (ARRAY['pending','paid','shipped','cancelled'])[(random()*3+1)::int],
       (random() * 20000 + 500)::int,
       now() - (random() * interval '90 days')
FROM generate_series(1, 40000) g;

INSERT INTO shop.order_items (order_id, product_id, quantity, unit_price_cents)
SELECT (random() * 39999 + 1)::bigint,
       (random() * 799 + 1)::bigint,
       (random() * 4 + 1)::int,
       (random() * 9000 + 100)::int
FROM generate_series(1, 120000) g;

INSERT INTO billing.invoices (order_id, amount_cents, paid_at, created_at)
SELECT o.id, o.total_cents,
       CASE WHEN o.status IN ('paid','shipped') THEN o.created_at + interval '1 hour' ELSE NULL END,
       o.created_at
FROM shop.orders o;

INSERT INTO analytics.events (user_id, event_type, payload, created_at)
SELECT (random() * 4999 + 1)::bigint,
       (ARRAY['page_view','add_to_cart','checkout','login'])[(random()*3+1)::int],
       jsonb_build_object('ip', '10.0.0.' || (random()*255)::int),
       now() - (random() * interval '30 days')
FROM generate_series(1, 60000) g;

ANALYZE;

-- Trigger some real seq scans + index scans so seq-scan-rate/dead-tuple
-- numbers on the Tables & Indexes tab aren't all zero.
DO $$
BEGIN
  FOR i IN 1..30 LOOP
    PERFORM count(*) FROM shop.orders WHERE user_id = (random()*4999+1)::bigint;
    PERFORM count(*) FROM analytics.events WHERE event_type = 'checkout';
    PERFORM * FROM shop.products WHERE price_cents > 5000;
  END LOOP;
END $$;

-- Some dead tuples + a never-vacuumed table for realistic coloring.
DELETE FROM shop.order_items WHERE id % 7 = 0;
UPDATE shop.orders SET status = 'paid' WHERE status = 'pending' AND id % 3 = 0;

-- A couple of real triggers, for the Triggers tab screenshot.
ALTER TABLE shop.orders ADD COLUMN updated_at TIMESTAMPTZ NOT NULL DEFAULT now();

CREATE OR REPLACE FUNCTION shop.set_updated_at() RETURNS trigger AS $$
BEGIN
  NEW.updated_at = now();
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER orders_set_updated_at
BEFORE UPDATE ON shop.orders
FOR EACH ROW EXECUTE FUNCTION shop.set_updated_at();

CREATE OR REPLACE FUNCTION billing.log_invoice_paid() RETURNS trigger AS $$
BEGIN
  IF NEW.paid_at IS NOT NULL AND OLD.paid_at IS NULL THEN
    INSERT INTO analytics.events (user_id, event_type, payload)
    VALUES (NULL, 'invoice_paid', jsonb_build_object('invoice_id', NEW.id));
  END IF;
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER invoices_log_paid
AFTER UPDATE ON billing.invoices
FOR EACH ROW EXECUTE FUNCTION billing.log_invoice_paid();
