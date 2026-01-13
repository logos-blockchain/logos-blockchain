<template>
  <div class="send-money-card flex flex-col p-6 sm:p-8 lg:p-10 
    rounded-2xl transition-all duration-300 w-full max-w-none border border-gray-100 dark:border-gray-400">
    
    <div class="flex items-center justify-between mb-6 border-b pb-4 border-gray-200 dark:border-gray-700">
      <h2 class="text-2xl sm:text-3xl font-bold text-gray-900 dark:text-white">
        Send MEM
      </h2>

      <div class="flex items-center space-x-1 bg-gray-50 dark:bg-gray-800/50 p-1 rounded-xl">
        <button 
          @click="prevRecipient"
          class="p-2 rounded-lg hover:bg-white dark:hover:bg-gray-700 hover:shadow-sm transition-all group"
          title="Previous Character"
        >
          <svg xmlns="http://www.w3.org/2000/svg" class="h-5 w-5 text-gray-400 group-hover:text-gray-900 dark:group-hover:text-white" fill="none" viewBox="0 0 24 24" stroke="currentColor">
            <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M15 19l-7-7 7-7" />
          </svg>
        </button>

        <button 
          @click="randomRecipient"
          class="p-2 rounded-lg hover:bg-white dark:hover:bg-gray-700 hover:shadow-sm transition-all group"
          title="Random Character"
        >
          <svg xmlns="http://www.w3.org/2000/svg" class="h-5 w-5 text-gray-400 group-hover:text-gray-900 dark:group-hover:text-white" fill="none" viewBox="0 0 24 24" stroke="currentColor">
            <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M4 4v5h.582m15.356 2A8.001 8.001 0 004.582 9m0 0H9m11 11v-5h-.581m0 0a8.003 8.003 0 01-15.357-2m15.357 2H15" />
          </svg>
        </button>

        <button 
          @click="nextRecipient"
          class="p-2 rounded-lg hover:bg-white dark:hover:bg-gray-700 hover:shadow-sm transition-all group"
          title="Next Character"
        >
          <svg xmlns="http://www.w3.org/2000/svg" class="h-5 w-5 text-gray-400 group-hover:text-gray-900 dark:group-hover:text-white" fill="none" viewBox="0 0 24 24" stroke="currentColor">
            <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M9 5l7 7-7 7" />
          </svg>
        </button>
      </div>
    </div>

    <form @submit.prevent="handleTransfer" class="space-y-6">
      
      <div class="flex flex-col">
        <label class="text-sm font-semibold text-gray-500 dark:text-gray-400 mb-1">
          To Character
        </label>
        <p class="text-base sm:text-lg font-mono text-gray-700 dark:text-white h-8 flex items-center">
          {{ transferData.to || 'Select a recipient...' }}
        </p>
      </div>

      <div class="flex flex-col">
        <label class="text-sm font-semibold text-gray-500 dark:text-gray-400 mb-2">
          Amount (MEM)
        </label>
        <input 
          v-model.number="transferData.amount"
          type="number" 
          step="0.01"
          placeholder="0.00"
          class="w-full p-3 rounded-xl border border-gray-200 dark:border-gray-700 bg-transparent text-gray-900 dark:text-white focus:ring-2 focus:ring-gray-400 outline-none font-bold text-lg transition-all"
          required
        />
      </div>

      <div class="pt-4">
        <button 
          type="submit"
          class="w-full py-4 rounded-xl bg-gray-900 dark:bg-white text-white dark:text-gray-900 font-bold text-lg hover:opacity-90 active:scale-[0.98] transition-all shadow-lg"
        >
          Confirm Transfer
        </button>
      </div>
      
    </form>
  </div>
</template>

<script setup>
import { reactive, computed, onMounted } from 'vue';

const props = defineProps({
  fromAddress: {
    type: String,
    default: 'Tsubasa'
  },
  characters: {
    type: Array,
    default: () => ['Tsubasa', 'Hyuga', 'Misaki', 'Wakabayashi', 'Wakashimazu', 'Misugi', 'Matsuyama', 'Ishizaki']
  }
});

const emit = defineEmits(['transfer']);

const transferData = reactive({
  to: '',
  amount: null
});

const availableRecipients = computed(() => {
  return props.characters.filter(name => name !== props.fromAddress);
});

const nextRecipient = () => {
  const list = availableRecipients.value;
  const currentIndex = list.indexOf(transferData.to);
  const nextIndex = (currentIndex + 1) % list.length;
  transferData.to = list[nextIndex];
};

const prevRecipient = () => {
  const list = availableRecipients.value;
  const currentIndex = list.indexOf(transferData.to);
  const prevIndex = (currentIndex - 1 + list.length) % list.length;
  transferData.to = list[prevIndex];
};

const randomRecipient = () => {
  const list = availableRecipients.value;
  let newRecipient;
  do {
    newRecipient = list[Math.floor(Math.random() * list.length)];
  } while (newRecipient === transferData.to && list.length > 1);
  transferData.to = newRecipient;
};

onMounted(() => {
  if (availableRecipients.value.length > 0) {
    transferData.to = availableRecipients.value[0];
  }
});

const handleTransfer = () => {
  if (!transferData.to || !transferData.amount) return;
  emit('transfer', { to: transferData.to, amount: transferData.amount });
  transferData.amount = null; 
};
</script>
