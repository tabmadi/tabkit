CREATE TABLE t (
  id uuid,
  email text,
  note text
);
COMMENT ON COLUMN t.note IS 'pii:bogus';
