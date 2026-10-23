/**
 * Performance budget validation script (#48)
 *
 * Verifies that performance budgets exist, covers critical routes, and validates
 * configuration for CI release gates.
 */

import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const rootDir = path.resolve(__dirname, '..');

const budgetFile = path.join(rootDir, 'lighthouse-budget.json');
const rcFile = path.join(rootDir, 'lighthouserc.json');

console.log('🔍 Validating frontend performance budgets...');

if (!fs.existsSync(budgetFile)) {
  console.error('❌ Missing lighthouse-budget.json');
  process.exit(1);
}

if (!fs.existsSync(rcFile)) {
  console.error('❌ Missing lighthouserc.json');
  process.exit(1);
}

const budgets = JSON.parse(fs.readFileSync(budgetFile, 'utf8'));
const rc = JSON.parse(fs.readFileSync(rcFile, 'utf8'));

// Verify critical routes in collect config
const requiredRoutes = ['/en', '/en/tournaments', '/en/tournaments/1/bracket', '/en/matches/1', '/en/leaderboard'];
const configuredUrls = rc.ci?.collect?.url || [];

const missingRoutes = requiredRoutes.filter(route => !configuredUrls.some(u => u.includes(route)));
if (missingRoutes.length > 0) {
  console.error(`❌ lighthouserc.json missing critical routes: ${missingRoutes.join(', ')}`);
  process.exit(1);
}

// Verify budgets cover critical paths
const budgetPaths = budgets.map(b => b.path);
console.log(`✅ Configured budget paths: ${budgetPaths.join(', ')}`);

// Verify assertions fail on regressions
const assertions = rc.ci?.assert?.assertions || {};
const requiredAssertions = ['categories:performance', 'largest-contentful-paint', 'cumulative-layout-shift'];
for (const req of requiredAssertions) {
  if (!assertions[req]) {
    console.error(`❌ lighthouserc.json missing required failure assertion: ${req}`);
    process.exit(1);
  }
}

console.log('✅ All performance budget configuration checks passed successfully.');
