CREATE TABLE IF NOT EXISTS browse_contacts (
    owner TEXT NOT NULL, recipient TEXT NOT NULL, revision INTEGER NOT NULL,
    summary TEXT NOT NULL, details TEXT NOT NULL,
    PRIMARY KEY(owner, recipient)
);
CREATE TABLE IF NOT EXISTS browse_items (
    owner TEXT NOT NULL, recipient TEXT NOT NULL, position INTEGER NOT NULL,
    item TEXT NOT NULL, mail TEXT NOT NULL,
    search_body TEXT NOT NULL, search_subject TEXT NOT NULL, search_text TEXT,
    PRIMARY KEY(owner, recipient, position)
);
CREATE TABLE IF NOT EXISTS browse_dirty (
    owner TEXT NOT NULL, recipient TEXT NOT NULL,
    PRIMARY KEY(owner, recipient)
);
CREATE TABLE IF NOT EXISTS browse_meta (version INTEGER NOT NULL);
