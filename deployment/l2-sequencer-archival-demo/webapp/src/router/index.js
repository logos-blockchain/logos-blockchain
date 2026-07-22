import { createRouter, createWebHistory } from 'vue-router';
import { ArchiveView, WalletView } from '@/views';

const router = createRouter({
  history: createWebHistory(import.meta.env.BASE_URL),
  routes: [
    {
      path: '/',
      name: 'wallet',
      component: WalletView,
    },
    {
      path: '/archive',
      name: 'archive',
      component: ArchiveView,
    },
  ],
  scrollBehavior(to, from, savedPosition) {
    return { top: 0 }
  },
});

export default router;
