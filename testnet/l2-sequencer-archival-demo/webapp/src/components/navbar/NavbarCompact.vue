<template>
  <div class="fixed top-4 left-0 right-0 z-50 flex justify-center px-4">
    <nav class="flex items-center justify-between w-full px-4 sm:px-6 py-3 rounded-full bg-white dark:bg-black border border-gray-100 dark:border-gray-800 transition-all duration-300">
      
      <div class="flex items-center gap-8">
        <SpinningLogo class="h-7 w-7 sm:h-8 sm:w-8 flex-shrink-0 mr-8 sm:mr-16" />
        
        <div class="flex items-center gap-8 sm:gap-16 text-[11px] sm:text-sm font-bold uppercase tracking-wider">
          <RouterLink 
            to="/" 
            class="text-gray-500 hover:text-gray-900 dark:text-gray-400 dark:hover:text-white transition-colors whitespace-nowrap"
            active-class="text-gray-900 dark:text-white"
          >
            Wallet
          </RouterLink>

          <div class="h-4 w-[1px] bg-gray-200 dark:bg-gray-700"></div>

          <RouterLink 
            to="/archive" 
            class="text-gray-500 hover:text-gray-900 dark:text-gray-400 dark:hover:text-white transition-colors whitespace-nowrap"
            active-class="text-gray-900 dark:text-white"
          >
            Archive
          </RouterLink>
        </div>
      </div>

      <div class="flex items-center space-x-3 sm:space-x-5">
        <a
          href="https://github.com/logos-blockchain/logos-blockchain"
          class="hover:opacity-70 transition-opacity flex-shrink-0"
          target="_blank"
          rel="noopener noreferrer"
        >
          <ThemeImg><img :src="githubSrc" class="h-4 w-4 sm:h-5 sm:w-5"/></ThemeImg>
        </a>
        <ThemeSwitch class="flex-shrink-0" />
      </div>
    </nav>
  </div>
</template>

<script setup>
  import { RouterLink } from 'vue-router';
  import {
    ThemeImg,
    SpinningLogo,
    ThemeSwitch
  } from '@/components';
  import githubSrc from '@/assets/github-mark.svg';
</script>
