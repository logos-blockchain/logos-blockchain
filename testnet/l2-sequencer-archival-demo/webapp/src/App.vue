<template>
  <NavBar class="fixed top-0 pl-11 pr-11 left-0 right-0 z-10 h-16 mx-auto navbar" />

  <RouterView />
</template>

<script setup>
import { RouterView } from 'vue-router'
import NavBar from './components/navbar/NavBarCompact.vue'
</script>

<style scoped>
.navbar {
  max-width: var(--app-max-w);
  margin: 1.5rem auto;
}
</style>
