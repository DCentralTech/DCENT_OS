//  W9-H — Cypress runner config (closes R6 CI gap #2).
//
// W8-G shipped two e2e specs at cypress/e2e/{profile_import_wizard,
// restore_to_stock}.cy.ts but the dashboard package.json had no
// cypress dev-dep and no cypress.config.ts, so the wizard UI flow had
// zero automated coverage.
//
// Both specs stub backend traffic via cy.intercept, so vite preview
// alone is enough — no live dcentrald backend, no SSH to a live
// miner. Defaults below match `vite preview` behaviour from
// `DCENT_OS_Antminer/dashboard/package.json`.
//
// CI invocation (per .github/workflows/dashboard-e2e.yml):
//     npm run e2e
// which calls `start-server-and-test preview http://localhost:4173 cypress:run`.
//
// Local invocation:
//     npm run cypress:open    # interactive
//     npm run cypress:run     # headless

import { defineConfig } from "cypress";

export default defineConfig({
  e2e: {
    // `vite preview` defaults to :4173. The npm `e2e` script boots
    // `vite preview` first and only then runs cypress.
    baseUrl: "http://localhost:4173",
    specPattern: "cypress/e2e/**/*.cy.{ts,tsx,js,jsx}",
    // Shared default API stubs keep local/CI e2e runs off live miners. Specs
    // may still override any endpoint with more specific route fixtures.
    supportFile: "cypress/support/e2e.ts",
    // CI noise reduction — keep screenshots on failure for
    // operator triage but skip videos.
    video: false,
    screenshotOnRunFailure: true,
    // Generous default timeouts — the wizard has multi-step
    // animated transitions and `cy.intercept` polling.
    defaultCommandTimeout: 8000,
    requestTimeout: 8000,
    // Retry once on flake to absorb the first-paint race. Unit-test
    // mocks are deterministic so we keep retries low.
    retries: {
      runMode: 1,
      openMode: 0,
    },
  },
});
