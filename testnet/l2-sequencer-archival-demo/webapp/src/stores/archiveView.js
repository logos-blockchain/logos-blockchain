import { defineStore } from 'pinia';

export const useArchiveStore = defineStore('archive', {
  state: () => ({
    transactions: [],
    loading: false,
    pollingInterval: null,
  }),

  actions: {
    async fetchAllTransactions() {
      // Show loading only on first run
      if (this.transactions.length === 0) this.loading = true;
      
      const baseUrl = import.meta.env.VITE_API_BASE_URL || 'http://localhost:8080';
      
      try {
        // Updated URL to point to the character provided in your example
        const response = await fetch(`${baseUrl}/accounts/Wakashimazu?tx=true`);
        if (!response.ok) throw new Error('Failed to fetch archive');
        
        const data = await response.json();
        
        // FIXED LOGIC: Extract transactions directly from the response object
        if (data && data.transactions) {
          // Sort by 'index' descending to get newest transactions at the top
          this.transactions = data.transactions.sort((a, b) => b.index - a.index);
        }
      } catch (err) {
        console.error("Archive Polling Error:", err);
      } finally {
        this.loading = false;
      }
    },

    startPolling() {
      this.stopPolling();
      this.fetchAllTransactions();
      this.pollingInterval = setInterval(() => this.fetchAllTransactions(), 5000);
    },

    stopPolling() {
      if (this.pollingInterval) {
        clearInterval(this.pollingInterval);
        this.pollingInterval = null;
      }
    }
  }
});
