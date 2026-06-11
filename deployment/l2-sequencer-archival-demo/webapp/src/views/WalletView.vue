<template>
  <div class="archive flex flex-col min-h-screen pt-24 p-4 sm:p-6 lg:p-8">
    <div class="flex justify-center m-20 pb-20 sm:pt-20">
      <img 
        src="@/assets/graphics/tsubasa-lg.png" 
        alt="Logo" 
        class="w-[300px]"
      />
    </div>
    <header class="w-full grid grid-cols-1 md:grid-cols-2 gap-6 md:gap-8 items-start mb-8">
      <div class="w-full relative group">
        <WalletInfo 
          :walletAddress="walletStore.accountName"
          :walletAmount="walletStore.balance"
          @regenerate="walletStore.regenerateWallet"
          @next="walletStore.nextWallet"
          @prev="walletStore.prevWallet"
          class="max-w-none w-full"
        />
      </div>

      <div class="w-full relative group"> 
        <TransactionCreator @transfer="handleTransfer" class="max-w-none w-full" />
      </div>
    </header>
    
    <main class="w-full pb-12 pt-12">
      <TransactionList 
        :currentAccount="walletStore.accountName"
        :transactions="walletStore.transactions" 
        class="max-w-none w-full" 
      />
    </main>
  </div>
</template>

<script setup>
import { onMounted, onUnmounted } from 'vue';
import { useWalletStore } from '@/stores/walletView.js';
import { TransactionCreator, TransactionList, WalletInfo } from '@/components';

const walletStore = useWalletStore();

const handleTransfer = async (data) => {
  await walletStore.sendTransaction(data);
};

onMounted(() => {
  walletStore.startPolling();
});

onUnmounted(() => {
  walletStore.stopPolling();
});
</script>
