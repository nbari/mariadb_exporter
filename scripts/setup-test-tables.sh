#!/usr/bin/env bash
# Create test tables for schema collector testing
set -euo pipefail

HOST="${1:-127.0.0.1}"
PORT="${2:-3306}"
USER="${3:-root}"
PASS="${4:-root}"

echo "📊 Creating test database and tables..."

mysql -h "$HOST" -P "$PORT" -u "$USER" -p"$PASS" <<'EOF'
-- Create test database
CREATE DATABASE IF NOT EXISTS testdb;
USE testdb;

-- Create some test tables with data
CREATE TABLE IF NOT EXISTS users (
    id INT AUTO_INCREMENT PRIMARY KEY,
    username VARCHAR(50) NOT NULL,
    email VARCHAR(100),
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    INDEX idx_username (username),
    INDEX idx_created (created_at)
);

CREATE TABLE IF NOT EXISTS orders (
    id INT AUTO_INCREMENT PRIMARY KEY,
    user_id INT,
    amount DECIMAL(10,2),
    status VARCHAR(20),
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    INDEX idx_user (user_id),
    INDEX idx_status (status)
);

CREATE TABLE IF NOT EXISTS products (
    id INT AUTO_INCREMENT PRIMARY KEY,
    name VARCHAR(100),
    price DECIMAL(10,2),
    stock INT DEFAULT 0,
    INDEX idx_name (name)
);

-- Insert some sample data to make tables measurable
INSERT INTO users (username, email) VALUES
    ('alice', 'alice@example.com'),
    ('bob', 'bob@example.com'),
    ('charlie', 'charlie@example.com');

INSERT INTO products (name, price, stock) VALUES
    ('Widget', 19.99, 100),
    ('Gadget', 29.99, 50),
    ('Doohickey', 39.99, 25);

INSERT INTO orders (user_id, amount, status) VALUES
    (1, 19.99, 'completed'),
    (2, 29.99, 'pending'),
    (1, 39.99, 'completed');

SELECT 'Created testdb with 3 tables' AS Result;
SELECT TABLE_NAME, TABLE_ROWS, 
       ROUND((DATA_LENGTH + INDEX_LENGTH)/1024, 2) AS 'Size_KB'
FROM information_schema.tables 
WHERE TABLE_SCHEMA = 'testdb'
ORDER BY (DATA_LENGTH + INDEX_LENGTH) DESC;
EOF

echo "✅ Test tables created successfully!"
echo ""
echo "To verify schema metrics are collected, run:"
echo "  cargo run -- --collector.schema"
echo "  curl -s localhost:9306/metrics | grep mariadb_info_schema_table"
