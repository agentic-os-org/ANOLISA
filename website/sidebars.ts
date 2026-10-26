import {readFileSync} from 'node:fs';
import path from 'node:path';
import type {SidebarsConfig} from '@docusaurus/plugin-content-docs';

const sidebars: SidebarsConfig = {
  userGuide: JSON.parse(readFileSync(path.join(__dirname, '.generated/user-guide-sidebar.json'), 'utf8')),
  ...JSON.parse(readFileSync(path.join(__dirname, '.generated/guide-sidebars.json'), 'utf8')),
};

export default sidebars;
