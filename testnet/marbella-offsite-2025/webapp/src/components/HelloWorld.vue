<template>
  <div class="greetings flex flex-col items-center justify-center lg:items-start lg:justify-start">
    <div class="flex items-center justify-center lg:justify-start">
      <img :src="logoSrc" class="h-12 ps-5">
    </div>

    <h3 class="pt-3">
      {{ msg }}
    </h3>
  </div>
</template>

<script setup>
  import { ThemeImg } from '@/components/ui';
  import logoSrc from '@/assets/logo-color.svg';
  import logoTxtSrc from '@/assets/logo-txt.svg';

  defineProps({
    msg: {
      type: String,
      required: true,
    },
  });
</script>

<style scoped>
  h1 {
    font-weight: 500;
    font-size: 2.6rem;
    position: relative;
    top: -10px;
  }

  h3 {
    font-size: 1.2rem;
  }

  .greetings h1,
  .greetings h3 {
    text-align: center;
  }

  @media (min-width: 1024px) {
    .greetings h1,
    .greetings h3 {
      text-align: left;
    }
  }
</style>
