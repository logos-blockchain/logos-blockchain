import { defineStore } from 'pinia';

export const useArchiveStore = defineStore('archive', {
  state: () => ({
    transactions: [],
    loading: false,
    eventSource: null,
  }),

  actions: {
    startPolling() {
      // Prevent multiple connections
      if (this.eventSource) return;
      this.loading = true;
      this.eventSource = new EventSource("http://127.0.0.1:8090/block_stream");

      this.eventSource.onopen = () => {
        this.loading = false;
      };

      this.eventSource.onmessage = (event) => {
        try {
          const blockData = JSON.parse(event.data);
          
          if (blockData && blockData.transactions) {
            // Map through the new transactions and force confirmed to true
            const confirmedTransactions = blockData.transactions.map(tx => ({
              ...tx,
              confirmed: true
            }));

            // Prepend the updated transactions to the existing list
            this.transactions = [...confirmedTransactions, ...this.transactions];
            
            // Keep memory clean (limit to 100 txs)
            if (this.transactions.length > 100) {
              this.transactions = this.transactions.slice(0, 100);
            }
          }
        } catch (err) {
          console.error("❌ Error parsing block data:", err);
        }
      };

      this.eventSource.onerror = (err) => {
        this.stopPolling();
        setTimeout(() => this.startPolling(), 5000);
      };
    },

    stopPolling() {
      if (this.eventSource) {
        this.eventSource.close();
        this.eventSource = null;
        console.log("🔌 Archive Stream Disconnected");
      }
    }
  }
});
