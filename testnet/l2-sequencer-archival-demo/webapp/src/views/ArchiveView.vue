<template>
  <div class="archive flex flex-col min-h-screen pt-24 p-4 sm:p-6 lg:p-8">
    
    <header class="w-full grid grid-cols-1 md:grid-cols-2 gap-6 md:gap-12 items-center mb-12 pt-20 pb-10">
      
      <div class="w-full">
        <div class="flex flex-col p-6 sm:p-8 lg:p-10 rounded-2xl border border-gray-100 dark:border-gray-400 bg-white dark:bg-black transition-all duration-300 ">
          
          <div class="flex items-center justify-between mb-6 border-b pb-4 border-gray-200 dark:border-gray-700">
            <h2 class="text-2xl sm:text-3xl font-bold text-gray-900 dark:text-white">
              Archive Explorer
            </h2>
          </div>

          <div class="space-y-6">
            <div class="flex flex-col">
              <span class="text-sm font-semibold text-gray-500 dark:text-gray-400 mb-1">
                Data Source
              </span>
              <p class="text-base sm:text-lg font-mono text-gray-700 dark:text-white">
                Testnet Archiver
              </p>
            </div>

            <div class="flex flex-col pt-4">
              <span class="text-sm font-semibold text-gray-500 dark:text-gray-400 mb-3">
                Feed Status
              </span>
              <div class="flex items-center space-x-3 bg-gray-50 dark:bg-gray-800/50 px-4 py-2 rounded-xl border border-gray-100 dark:border-gray-800 w-fit">
                <span class="relative flex h-2 w-2">
                  <span class="animate-ping absolute inline-flex h-full w-full rounded-full bg-green-400 opacity-75"></span>
                  <span class="relative inline-flex rounded-full h-2 w-2 bg-green-500"></span>
                </span>
                <p class="text-gray-500 dark:text-gray-400 font-mono uppercase tracking-[0.1em] text-[10px] font-bold">
                  Live • Refreshing 5s
                </p>
              </div>
            </div>
          </div>
        </div>
      </div>
      
      <div class="flex justify-center md:justify-start">
        <img 
          src="@/assets/graphics/tsubasa-archive.png" 
          alt="Logo" 
          class="w-[280px] sm:w-[380px] object-contain "
        />
      </div>
    </header>

    <main class="w-full pb-24">
      <div v-if="archiveStore.loading" class="flex justify-center py-20">
        <div class="h-8 w-8 border-2 border-gray-900 dark:border-white border-t-transparent rounded-full animate-spin"></div>
      </div>

      <div v-else>
        <TransactionList 
          :transactions="archiveStore.transactions" 
          currentAccount="ARCHIVE_VIEW"
          class="max-w-none w-full" 
        />
      </div>
    </main>
  </div>
</template>

<script setup>
import { onMounted, onUnmounted } from 'vue';
import { useArchiveStore } from '@/stores/archiveView';
import { TransactionList } from '@/components';

const archiveStore = useArchiveStore();
</script>
