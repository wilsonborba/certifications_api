-- Legacy plan_waitlist rows are not migrated; drop any legacy local waitlist/plans tables.
DROP TABLE IF EXISTS plan_waitlist;
DROP TABLE IF EXISTS waitlist;
DROP TABLE IF EXISTS plans;
DROP TABLE IF EXISTS support_tickets;
