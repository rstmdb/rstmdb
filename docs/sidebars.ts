import type {SidebarsConfig} from '@docusaurus/plugin-content-docs';

const sidebars: SidebarsConfig = {
  docsSidebar: [
    'intro',
    'getting-started',
    'architecture',
    {
      type: 'category',
      label: 'Concepts',
      items: [
        'concepts/state-machines',
        'concepts/instances',
        'concepts/events',
        'concepts/guards',
        'concepts/wal',
      ],
    },
    {
      type: 'category',
      label: 'Protocol',
      items: [
        'protocol/overview',
        'protocol/wire-format',
        'protocol/messages',
        'protocol/errors',
      ],
    },
    {
      type: 'category',
      label: 'API Reference',
      items: [
        'api/commands',
        'api/session',
        'api/machines',
        'api/instances',
        'api/events',
        'api/subscriptions',
        'api/storage',
      ],
    },
    'cli',
    'configuration',
    {
      type: 'category',
      label: 'Operations',
      items: [
        'operations/deployment',
        'operations/docker',
        'operations/monitoring',
        'operations/backup-recovery',
        'operations/security',
      ],
    },
    {
      type: 'category',
      label: 'Client Libraries',
      items: [
        'clients/overview',
        'clients/rust',
        'clients/python',
        'clients/go',
        'clients/typescript',
      ],
    },
    'roadmap',
  ],
};

export default sidebars;
