-- Setup script for mariadb_exporter user
-- Run this script as root or a user with GRANT privileges
--
-- For socket connection (recommended):
--   mysql -uroot -p < setup-exporter-user.sql
--
-- This creates a secure exporter user with minimal privileges

-- Create user for local socket connection (no password needed)
CREATE USER IF NOT EXISTS 'exporter'@'localhost' IDENTIFIED BY '';

-- Grant minimal required permissions
-- SELECT: Read table data and system tables
-- PROCESS: View server processes (SHOW PROCESSLIST)
-- REPLICATION CLIENT: View replication status (SHOW SLAVE STATUS)
GRANT SELECT, PROCESS, REPLICATION CLIENT ON *.* TO 'exporter'@'localhost';

-- Limit concurrent connections (matches exporter connection pool: 1-3 connections)
ALTER USER 'exporter'@'localhost' WITH MAX_USER_CONNECTIONS 3;

-- Apply changes
FLUSH PRIVILEGES;

-- Verify the user was created correctly
SELECT User, Host, max_user_connections
FROM mysql.user
WHERE User = 'exporter' AND Host = 'localhost';

-- Show granted privileges
SHOW GRANTS FOR 'exporter'@'localhost';
