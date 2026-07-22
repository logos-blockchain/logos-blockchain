<template>
  <div class="w-auto" :class="{ 'invert': invertColor }">
    <slot></slot>
  </div>
</template>

<script setup>
  const props = defineProps({
    invertColor: {
      type: Boolean,
      default: true,
    },
  });
</script>

<style scoped>
  .invert {
    filter: none;
  }
  .dark .invert {
    filter: invert(100%);
  }
</style>
