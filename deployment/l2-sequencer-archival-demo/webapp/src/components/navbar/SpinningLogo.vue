<template>
  <RouterLink
      to="/"
      active-class="noop-link"
      exact-active-class="noop-link"
      class="noop-link">
    <ThemeImg>
      <img :src="logoSrc" :style="{ height: '20px', transform: `rotate(${logoRotation}deg)` }">
    </ThemeImg>
  </RouterLink>
</template>

<script setup>
  import { ref, onMounted, onUnmounted } from 'vue';
  import { ThemeImg } from '@/components/ui';
  import logoSrc from '@/assets/logo.svg';

  const logoRotation = ref(0);

  let lastScrollTop = 0;
  const handleScroll = () => {
    const st = window.pageYOffset || document.documentElement.scrollTop;
    const scrollDelta = st - lastScrollTop;
    lastScrollTop = st <= 0 ? 0 : st;
    logoRotation.value += scrollDelta * 0.9;
  };

  onMounted(() => {
    window.addEventListener('scroll', handleScroll);
  });

  onUnmounted(() => {
    window.removeEventListener('scroll', handleScroll);
  });
</script>
