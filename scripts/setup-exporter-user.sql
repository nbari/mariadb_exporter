-- Setup script for mariadb_exporter user
-- Run this script as root or a user with GRANT privileges
--
-- For socket connection (recommended):
--   mariadb -uroot -p < setup-exporter-user.sql
--
-- This creates a secure exporter user with minimal privileges

-- Create user for local socket connection with connection limit
CREATE USER IF NOT EXISTS 'exporter'@'localhost' IDENTIFIED BY '' WITH MAX_USER_CONNECTIONS 3;

-- Grant minimal required permissions
-- SELECT: Read table data and system tables
-- PROCESS: View server processes (SHOW PROCESSLIST)
-- REPLICATION CLIENT: View replication status (SHOW SLAVE STATUS)
GRANT SELECT, PROCESS, REPLICATION CLIENT ON *.* TO 'exporter'@'localhost';

-- Apply changes
FLUSH PRIVILEGES;

-- Verify the user was created correctly
SELECT User, Host, max_user_connections
FROM mysql.user
WHERE User = 'exporter' AND Host = 'localhost';

-- Show granted privileges
SHOW GRANTS FOR 'exporter'@'localhost';
