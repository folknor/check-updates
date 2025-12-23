#!/bin/bash
# Test script for the detector implementation

echo "Running detector tests..."
cargo test --lib detector::tests

echo ""
echo "Running cargo check..."
cargo check
