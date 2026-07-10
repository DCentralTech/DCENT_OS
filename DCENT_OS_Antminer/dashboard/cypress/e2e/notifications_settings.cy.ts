/// <reference types="cypress" />

const supportedEvents = [
  'emergency_shutdown',
  'fan_failure',
  'pool_disconnected',
  'mining_stopped',
  'hashboard_offline',
  'thermal_restart',
];

function webhookConfig(overrides: Record<string, unknown> = {}) {
  return {
    enabled: false,
    url: '',
    events: supportedEvents,
    supported_events: supportedEvents,
    restart_required: true,
    format: 'generic',
    telegram_bot_token: '',
    telegram_chat_id: '',
    ...overrides,
  };
}

function seedStandard(win: Window) {
  win.localStorage.clear();
  win.localStorage.setItem('dcentos-settings', JSON.stringify({
    setupComplete: true,
    mode: 'standard',
    minerName: 'Notification Miner',
  }));
  win.localStorage.setItem('dcentos-current-page', 'settings/network');
  win.localStorage.setItem('dcentos-nav-standard', 'settings/network');
}

function stubNetworkTab() {
  cy.intercept('GET', '/api/network/info', {
    statusCode: 200,
    body: {
      hostname: 'notify-miner',
      mac: '00:11:22:33:44:55',
      primary_interface: 'eth0',
      ipv4: '203.0.113.50',
      ipv4_cidr: '203.0.113.50/24',
      gateway: '203.0.113.1',
      dns: '1.1.1.1',
      link_state: 'up',
      dhcp: true,
      warnings: [],
    },
  });
}

describe('Notification settings', () => {
  it('saves Telegram webhook settings with the daemon contract fields', () => {
    stubNetworkTab();
    cy.intercept('GET', '/api/config/webhook', {
      statusCode: 200,
      body: webhookConfig(),
    }).as('getWebhook');
    cy.intercept('POST', '/api/config/webhook', req => {
      expect(req.body).to.deep.include({
        enabled: true,
        format: 'telegram',
        telegram_bot_token: '123456789:ABCdefGh',
        telegram_chat_id: '-1001234567890',
      });
      expect(req.body.events).to.include('fan_failure');
      req.reply({
        statusCode: 200,
        body: {
          status: 'ok',
          message: 'Webhook settings saved. Restart daemon to use them.',
          config: webhookConfig({
            enabled: true,
            format: 'telegram',
            telegram_bot_token: '<redacted>',
            telegram_chat_id: '-1001234567890',
          }),
        },
      });
    }).as('saveWebhook');

    cy.visit('/#/settings/network', { onBeforeLoad: seedStandard });
    cy.wait('@getWebhook');
    cy.contains('.section-title', 'Notification Channels').scrollIntoView().should('be.visible');
    cy.get('#webhook-format').select('telegram');
    cy.contains('label', 'Alert notifications').find('input').check();
    cy.get('#webhook-telegram-token').type('123456789:ABCdefGh');
    cy.get('#webhook-telegram-chat').type('-1001234567890');
    cy.get('#webhook-format')
      .parents('.page-surface')
      .within(() => {
        cy.contains('button', 'Save').click();
      });

    cy.wait('@saveWebhook');
    cy.window().then(win => {
      const raw = win.localStorage.getItem('dcentos-notifications');
      expect(raw).to.be.a('string');
      expect(JSON.parse(raw ?? '{}').channels.webhook.enabled).to.equal(true);
    });
    cy.contains('Webhook settings saved. Restart daemon to use them.').should('be.visible');
  });

  it('renders daemon save errors without pretending delivery is configured', () => {
    stubNetworkTab();
    cy.intercept('GET', '/api/config/webhook', {
      statusCode: 200,
      body: webhookConfig({ enabled: true, format: 'discord', url: 'https://discord.example/hook' }),
    }).as('getWebhook');
    cy.intercept('POST', '/api/config/webhook', {
      statusCode: 400,
      body: {
        error: 'invalid_webhook',
        suggestion: 'Check the webhook URL and channel permissions.',
      },
    }).as('saveWebhookFail');

    cy.visit('/#/settings/network', { onBeforeLoad: seedStandard });
    cy.wait('@getWebhook');
    cy.contains('.section-title', 'Notification Channels').scrollIntoView().should('be.visible');
    cy.get('#webhook-url').clear().type('https://discord.example/bad');
    cy.get('#webhook-format')
      .parents('.page-surface')
      .within(() => {
        cy.contains('button', 'Save').click();
      });

    cy.wait('@saveWebhookFail');
    cy.contains('Failed to save webhook settings').should('be.visible');
    cy.contains('Webhook settings saved').should('not.exist');
  });
});
