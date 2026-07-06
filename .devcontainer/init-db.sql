-- Runs once on first MariaDB init (mounted into /docker-entrypoint-initdb.d/).
-- The compose service already creates root@'%' with the well-known dev password,
-- so this file just seeds an unprivileged `exporter` user (mirroring the
-- least-privilege user documented in the README) and a small test database so the
-- collectors and tests find data immediately.

-- Least-privilege exporter user, reachable from any host on the compose network.
CREATE USER IF NOT EXISTS 'exporter'@'%' IDENTIFIED BY '' WITH MAX_USER_CONNECTIONS 3;
GRANT SELECT, PROCESS, REPLICATION CLIENT ON *.* TO 'exporter'@'%';
FLUSH PRIVILEGES;

-- Minimal seed data so the schema collector has tables to report on.
CREATE DATABASE IF NOT EXISTS testdb;
USE testdb;

CREATE TABLE IF NOT EXISTS users (
    id INT AUTO_INCREMENT PRIMARY KEY,
    username VARCHAR(50) NOT NULL,
    email VARCHAR(100),
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    INDEX idx_username (username)
);

INSERT IGNORE INTO users (id, username, email) VALUES
    (1, 'alice', 'alice@example.com'),
    (2, 'bob', 'bob@example.com');
