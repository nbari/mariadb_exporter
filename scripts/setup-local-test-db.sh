#!/usr/bin/env bash
# Setup local test database with sample data
set -euo pipefail

HOST="${MARIADB_HOST:-127.0.0.1}"
PORT="${MARIADB_PORT:-3306}"
USER="${MARIADB_USER:-root}"
PASS="${MARIADB_PASS:-root}"

echo "📊 Setting up test database..."

# Wait for MariaDB to be ready
timeout 30 bash -c "until mysqladmin ping -h ${HOST} -P ${PORT} -u${USER} -p${PASS} --silent 2>/dev/null; do sleep 1; done" || {
    echo "❌ MariaDB not responding on ${HOST}:${PORT}"
    exit 1
}

# Create test database and tables
mysql -h "$HOST" -P "$PORT" -u "$USER" -p"$PASS" <<'EOF' 2>/dev/null || true
-- Create test database
CREATE DATABASE IF NOT EXISTS testdb;
USE testdb;

-- Create test tables with data (for schema collector)
CREATE TABLE IF NOT EXISTS users (
    id INT AUTO_INCREMENT PRIMARY KEY,
    username VARCHAR(50) NOT NULL,
    email VARCHAR(100),
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    INDEX idx_username (username)
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
    stock INT DEFAULT 0
);

-- Insert sample data if tables are empty
INSERT IGNORE INTO users (id, username, email) VALUES
    (1, 'alice', 'alice@example.com'),
    (2, 'bob', 'bob@example.com'),
    (3, 'charlie', 'charlie@example.com');

INSERT IGNORE INTO products (id, name, price, stock) VALUES
    (1, 'Widget', 19.99, 100),
    (2, 'Gadget', 29.99, 50),
    (3, 'Doohickey', 39.99, 25);

INSERT IGNORE INTO orders (id, user_id, amount, status) VALUES
    (1, 1, 19.99, 'completed'),
    (2, 2, 29.99, 'pending'),
    (3, 1, 39.99, 'completed');
EOF

echo "✅ Test database setup complete!"
