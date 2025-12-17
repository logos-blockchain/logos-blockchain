<template>
  <div class="flex justify-between items-center p-4 navbar-background">
    <SpinningLogo />
    <div class="flex items-center justify-center space-x-4">
      <a
          href="https://github.com/logos-blockchain/logos-blockchain"
          class="noop-link max-h-24"
          target="_blank"
          rel="noopener noreferrer"
          >
          <ThemeImg><img :src="githubSrc" class="max-h-5"/></ThemeImg>
      </a>
      <ThemeSwitch />
    </div>
  </div>
</template>

<script setup>
  import { ref } from 'vue';
  import { RouterLink } from 'vue-router';
  import {
    TransitionRoot,
    TransitionChild,
  } from '@headlessui/vue'
  import {
    ThemeImg,
    SpinningLogo,
    ThemeSwitch
  } from '@/components';
  import githubSrc from '@/assets/github-mark.svg';
</script>

<style scoped>
  .noop-link {
    background-color: none;
  }
  .noop-link:hover {
    background-color: transparent;
  }
  .navbar-background {
  }
</style>

