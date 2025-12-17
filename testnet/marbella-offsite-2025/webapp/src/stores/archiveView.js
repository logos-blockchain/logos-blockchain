import { defineStore } from 'pinia';

export const useProjectsStore = defineStore('projects', {
  state: () => ({
    projects: [],
  }),
  actions: {
    async fetchData() {
      // const response = await fetch('your-api-endpoint');
      // this.projects = await response.json();

      this.projects = [p, f, p, p, p, p];
    },
  },
});

var p = {
  title: 'memet',
  brief: 'test test test',
  url: 'voo.fm',
  repo: 'https://github.com/bacv/voo-fm',
  logo: '',
  screens: [''],
  desc: 'More ......',
  tags: ['ci', 'devops', 'mastodon'],
}

var f = {
  title: 'voo.fm',
  brief: 'test test test',
  url: 'voo.fm',
  repo: 'https://github.com/bacv/voo-fm',
  logo: '',
  screens: [''],
  desc: 'More ......',
  tags: ['ci', 'devops', 'mastodon'],
  featured: true,
}
