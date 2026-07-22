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
                    <span 
                      :class="statusClasses.ping"
                      class="absolute inline-flex h-full w-full rounded-full opacity-75"
                    ></span>
                    <span 
                      :class="statusClasses.dot"
                      class="relative inline-flex rounded-full h-2 w-2"
                    ></span>
                  </span>

                  <p class="text-gray-500 dark:text-gray-400 font-mono uppercase tracking-[0.1em] text-[10px] font-bold">
                    {{ statusText }}
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
      <ArchivedBlockList 
        :blocks="archiveStore.blocks"
        @openBlock="archiveStore.openBlockInExplorer"
        @openTransaction="archiveStore.openTransactionInExplorer"
      />
    </main>
  </div>
</template>

<script setup>
import { onMounted, onUnmounted, computed } from 'vue';
import { useArchiveStore } from '@/stores/archiveView';
import { ArchivedBlockList } from '@/components';

const archiveStore = useArchiveStore();

const statusClasses = computed(() => {
  const isConnected = archiveStore.connectionStatus === 'connected';
  const isError = archiveStore.connectionStatus === 'error';

  return {
    dot: isConnected ? 'bg-green-500' : (isError ? 'bg-red-500' : 'bg-yellow-500'),
    ping: isConnected ? 'animate-ping bg-green-400' : (isError ? 'bg-red-400' : 'bg-yellow-400')
  };
});

const statusText = computed(() => {
  switch (archiveStore.connectionStatus) {
    case 'connected':
      return 'Live • Stream Active';
    case 'waiting':
      return 'Waiting...';
    case 'error':
      return 'Offline • Reconnecting';
    default:
      return 'Stream Disconnected';
  }
});

onMounted(async () => {
  await archiveStore.fetchCachedBlocks();
  archiveStore.startStream()
})

onUnmounted(() => {
  archiveStore.stopStream()
})
</script>
