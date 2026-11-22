# ArenaX Development Makefile

.PHONY: help install build test clean lint format check-all docker-build docker-up docker-down bindings bindings-check

# Default target
help:
	@echo "ArenaX Development Commands:"
	@echo ""
	@echo "Installation:"
	@echo "  install          Install all dependencies"
	@echo "  install-frontend Install frontend dependencies"
	@echo "  install-backend  Install backend dependencies"
	@echo "  install-contracts Install contracts dependencies"
	@echo ""
	@echo "Development:"
	@echo "  dev              Start all services in development mode"
	@echo "  dev-frontend     Start frontend development server"
	@echo "  dev-backend      Start backend development server"
	@echo ""
	@echo "Building:"
	@echo "  build            Build all projects"
	@echo "  build-frontend   Build frontend for production"
	@echo "  build-backend    Build backend"
	@echo "  build-contracts  Build contracts for deployment"
	@echo ""
	@echo "Testing:"
	@echo "  test             Run all tests"
	@echo "  test-frontend    Run frontend tests"
	@echo "  test-backend     Run backend tests"
	@echo "  test-contracts   Run contracts tests"
	@echo ""
	@echo "Code Quality:"
	@echo "  lint             Lint all code"
	@echo "  format           Format all code"
	@echo "  check-all        Run all checks (lint, format, test)"
	@echo ""
	@echo "Bindings:"
	@echo "  bindings         Generate typed Rust + TypeScript client bindings"
	@echo "  bindings-check   Verify generated bindings are current (CI)"
	@echo ""
	@echo "Docker:"
	@echo "  docker-build     Build Docker images"
	@echo "  docker-up        Start services with Docker Compose"
	@echo "  docker-down      Stop Docker services"
	@echo ""
	@echo "Utilities:"
	@echo "  clean            Clean all build artifacts"
	@echo "  setup            Initial project setup"

# Installation
install: install-frontend install-backend install-contracts

install-frontend:
	@echo "Installing frontend dependencies..."
	@if command -v yarn >/dev/null 2>&1; then cd frontend && yarn install; else echo "Yarn not found, skipping frontend installation"; fi

install-backend:
	@echo "Installing backend dependencies..."
	@if [ -f "backend/Cargo.toml" ]; then cd backend && cargo build; else echo "Backend Cargo.toml not found, skipping backend installation"; fi

install-contracts:
	@echo "Installing contracts dependencies..."
	cd contracts && cargo build

# Development
dev:
	@echo "Starting all services..."
	@echo "Run 'make dev-frontend' and 'make dev-backend' in separate terminals"

dev-frontend:
	@echo "Starting frontend development server..."
	@if command -v yarn >/dev/null 2>&1; then cd frontend && yarn dev; else echo "Yarn not found, cannot start frontend"; fi

dev-backend:
	@echo "Starting backend development server..."
	cd backend && cargo run

# Building
build: build-frontend build-backend build-contracts

build-frontend:
	@echo "Building frontend..."
	cd frontend && yarn build

build-backend:
	@echo "Building backend..."
	cd backend && cargo build --release

build-contracts:
	@echo "Installing WASM target..."
	rustup target add wasm32-unknown-unknown
	@echo "Building contracts..."
	cd contracts && cargo build --target wasm32-unknown-unknown --release

# Typed client bindings (Issue #226)
# Generated from the checked-in interface artifact at contracts/interface.json.
# Bindings are versioned and pin the network + contract version so consumers
# cannot accidentally mix versions.
BINDINGS_ARTIFACT ?= contracts/interface.json
BINDINGS_OUT ?= bindings

bindings:
	@echo "Generating typed client bindings from $(BINDINGS_ARTIFACT)..."
	@if [ ! -f "$(BINDINGS_ARTIFACT)" ]; then echo "Missing interface artifact: $(BINDINGS_ARTIFACT)"; exit 1; fi
	@python3 scripts/generate_bindings.py --artifact "$(BINDINGS_ARTIFACT)" --out "$(BINDINGS_OUT)"
	@echo "Bindings written to $(BINDINGS_OUT)/"

bindings-check:
	@echo "Verifying generated bindings are current..."
	@if [ ! -f "$(BINDINGS_ARTIFACT)" ]; then echo "Missing interface artifact: $(BINDINGS_ARTIFACT)"; exit 1; fi
	@python3 scripts/generate_bindings.py --artifact "$(BINDINGS_ARTIFACT)" --out "$(BINDINGS_OUT)" --check

# Testing
test: test-issue-1158

test-issue-1158:
	@echo "Running Issue #1158 verification tests..."
	@python3 -m unittest discover -s tests -p "test_issue_*.py"

test-frontend:
	@echo "Running frontend tests..."
	@if [ -f "frontend/node_modules/.bin/jest" ]; then cd frontend && yarn test; else echo "Frontend dependencies not installed, skipping frontend tests"; fi

test-backend:
	@echo "Running backend tests..."
	@if [ -f "backend/Cargo.toml" ]; then cd backend && cargo test; else echo "Backend Cargo.toml not found, skipping backend tests"; fi

test-contracts:
	@echo "Running contracts tests..."
	@if [ -f "contracts/Cargo.toml" ]; then cd contracts && cargo test; else echo "Contracts Cargo.toml not found, skipping contracts tests"; fi

# Code Quality
lint:
	@echo "Linting frontend..."
	cd frontend && yarn lint
	@echo "Linting backend..."
	cd backend && cargo clippy
	@echo "Linting contracts..."
	cd contracts && cargo clippy

format:
	@echo "Formatting frontend..."
	cd frontend && yarn format
	@echo "Formatting backend..."
	cd backend && cargo fmt
	@echo "Formatting contracts..."
	cd contracts && cargo fmt

check-all: lint format test bindings-check
	@echo "All checks completed!"

# Docker
docker-build:
	@echo "Building Docker images..."
	@docker-compose build

docker-up:
	@echo "Starting services with Docker Compose..."
	@docker-compose up -d

docker-down:
	@echo "Stopping services with Docker Compose..."
	@docker-compose down

# Utilities
clean:
	@echo "Cleaning build artifacts..."
	@if [ -d "frontend" ]; then cd frontend && rm -rf .next node_modules; fi
	@if [ -f "backend/Cargo.toml" ]; then cd backend && cargo clean; fi
	@if [ -f "contracts/Cargo.toml" ]; then cd contracts && cargo clean; fi
	@rm -rf target

setup: install
	@echo "Setting up environment files..."
	@if [ -f "frontend/env.example" ] && [ ! -f "frontend/.env.local" ]; then cp frontend/env.example frontend/.env.local; fi
	@if [ -f "backend/env.example" ] && [ ! -f "backend/.env" ]; then cp backend/env.example backend/.env; fi
	@if [ -f "contracts/env.example" ] && [ ! -f "contracts/.env" ]; then cp contracts/env.example contracts/.env; fi
	@echo "Setup complete! Edit the .env files with your configuration."
