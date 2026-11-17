CREATE TABLE IF NOT EXISTS sent_mail (
            owner TEXT NOT NULL, fingerprint TEXT NOT NULL, message_id TEXT NOT NULL,
            sender TEXT NOT NULL, recipient TEXT NOT NULL, role TEXT NOT NULL,
            relationship TEXT NOT NULL, purpose TEXT NOT NULL, authored_body TEXT NOT NULL,
            PRIMARY KEY (owner, fingerprint, recipient)
        );
        CREATE INDEX IF NOT EXISTS mail_purpose ON sent_mail(owner, purpose);
        CREATE INDEX IF NOT EXISTS mail_recipient ON sent_mail(owner, recipient);
        CREATE INDEX IF NOT EXISTS mail_role ON sent_mail(owner, role);
        CREATE INDEX IF NOT EXISTS mail_relationship ON sent_mail(owner, relationship);
        CREATE TABLE IF NOT EXISTS mail_archive (
            id INTEGER PRIMARY KEY AUTOINCREMENT, owner TEXT NOT NULL, fingerprint TEXT NOT NULL,
            subject TEXT NOT NULL, sent_at TEXT NOT NULL, sender TEXT NOT NULL,
            to_json TEXT NOT NULL, cc_json TEXT NOT NULL, original_body TEXT NOT NULL,
            original_authored_body TEXT NOT NULL, source_name TEXT NOT NULL,
            revision INTEGER NOT NULL DEFAULT 0, updated_at TEXT,
            UNIQUE(owner, fingerprint)
        );
        CREATE INDEX IF NOT EXISTS archive_date ON mail_archive(sent_at DESC, id DESC);
        CREATE TABLE IF NOT EXISTS contact_labels (
            owner TEXT NOT NULL, recipient TEXT NOT NULL, label TEXT NOT NULL,
            revision INTEGER NOT NULL DEFAULT 1, PRIMARY KEY(owner, recipient)
        );
        CREATE TABLE IF NOT EXISTS label_colors (
            label TEXT PRIMARY KEY, color TEXT NOT NULL,
            revision INTEGER NOT NULL DEFAULT 1
        );
        CREATE TABLE IF NOT EXISTS deleted_messages (
            owner TEXT NOT NULL, message_key TEXT NOT NULL,
            PRIMARY KEY(owner, message_key)
        );
        CREATE TABLE IF NOT EXISTS manual_conversations (
            owner TEXT NOT NULL, recipient TEXT NOT NULL, name TEXT NOT NULL,
            created_at TEXT NOT NULL, PRIMARY KEY(owner, recipient)
        );
        CREATE TABLE IF NOT EXISTS received_replies (
            id INTEGER PRIMARY KEY AUTOINCREMENT, owner TEXT NOT NULL, recipient TEXT NOT NULL,
            body TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS received_conversation ON received_replies(owner, recipient);
        CREATE TABLE IF NOT EXISTS general_context (
            id INTEGER PRIMARY KEY CHECK(id=1), entries_json TEXT NOT NULL,
            revision INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS incoming_mail (
            owner TEXT NOT NULL, fingerprint TEXT NOT NULL, message_id TEXT NOT NULL,
            sender TEXT NOT NULL, recipient TEXT NOT NULL, role TEXT NOT NULL,
            relationship TEXT NOT NULL, purpose TEXT NOT NULL, authored_body TEXT NOT NULL,
            PRIMARY KEY(owner, fingerprint, recipient)
        );
        CREATE INDEX IF NOT EXISTS incoming_recipient ON incoming_mail(owner, recipient);
        CREATE VIEW IF NOT EXISTS conversation_mail AS
            SELECT * FROM sent_mail UNION ALL SELECT * FROM incoming_mail;
        CREATE TABLE IF NOT EXISTS gmail_sync (
            account TEXT PRIMARY KEY, owner TEXT NOT NULL, history_id TEXT NOT NULL DEFAULT '',
            synced_at TEXT
        );
        CREATE TABLE IF NOT EXISTS gmail_messages (
            account TEXT NOT NULL, gmail_id TEXT NOT NULL, owner TEXT NOT NULL,
            fingerprint TEXT NOT NULL DEFAULT '', dedup_key TEXT NOT NULL DEFAULT '', mail_json TEXT, recipients_json TEXT,
            status TEXT NOT NULL, error TEXT, attempts INTEGER NOT NULL DEFAULT 0,
            retry_at INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY(account, gmail_id)
        );
        CREATE INDEX IF NOT EXISTS gmail_pending ON gmail_messages(account, status, retry_at);
        CREATE INDEX IF NOT EXISTS gmail_dedup ON gmail_messages(owner, dedup_key);
