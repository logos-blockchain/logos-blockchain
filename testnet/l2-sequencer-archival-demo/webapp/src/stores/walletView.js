import { defineStore } from 'pinia';
import { sequencerApi } from '@/api/sequencer';

const TSUBASA_NAMES = [
 'Tsubasa', 
 'Hyuga', 
 'Misaki', 
 'Wakabayashi', 
 'Wakashimazu', 
 'Misugi', 
 'Matsuyama', 
 'Ishizaki'
];

export const useWalletStore = defineStore('wallet', {
  state: () => ({
    accountName: '', 
    balance: 0,
    confirmedBalance: 0,
    transactions: [],
    pollingInterval: null,
  }),

  actions: {
    getCookie(name) {
      const value = `; ${document.cookie}`;
      const parts = value.split(`; ${name}=`);
      if (parts.length === 2) return parts.pop().split(';').shift();
      return null;
    },

    setCookie(name, value, days = 7) {
      const date = new Date();
      date.setTime(date.getTime() + (days * 24 * 60 * 60 * 1000));
      const expires = "expires=" + date.toUTCString();
      document.cookie = `${name}=${value}; ${expires}; path=/`;
    },

    // ... inside actions ...

    regenerateWallet() {
      document.cookie = "sequencer_wallet_name=; expires=Thu, 01 Jan 1970 00:00:00 UTC; path=/;";
      this.stopPolling();
      this.initializeWallet();
      this.startPolling();
    },

    nextWallet() {
      const currentIndex = TSUBASA_NAMES.indexOf(this.accountName);
      const nextIndex = (currentIndex + 1) % TSUBASA_NAMES.length;
      this.updateWallet(TSUBASA_NAMES[nextIndex]);
    },

    prevWallet() {
      const currentIndex = TSUBASA_NAMES.indexOf(this.accountName);
      const prevIndex = (currentIndex - 1 + TSUBASA_NAMES.length) % TSUBASA_NAMES.length;
      this.updateWallet(TSUBASA_NAMES[prevIndex]);
    },

    updateWallet(newName) {
      this.stopPolling();
      this.accountName = newName;
      this.setCookie('sequencer_wallet_name', newName);
      this.startPolling();
    },

    initializeWallet() {
      const savedName = this.getCookie('sequencer_wallet_name');
      
      if (savedName) {
        this.accountName = savedName;
      } else {
        const randomName = TSUBASA_NAMES[Math.floor(Math.random() * TSUBASA_NAMES.length)];
        this.accountName = randomName;
        this.setCookie('sequencer_wallet_name', randomName);
      }
    },

    async fetchAccountData() {
      if (!this.accountName) return;
      try {
        const data = await sequencerApi.getAccount(this.accountName);
        this.balance = data.balance;
        this.confirmedBalance = data.confirmed_balance;
        this.transactions = data.transactions || [];
      } catch (err) {
        console.error('Polling error:', err);
      }
    },

    async sendTransaction(payload) {
      try {
        await sequencerApi.postTransfer(this.accountName, payload.to, payload.amount);
        await this.fetchAccountData();
      } catch (err) {
        alert(err.message);
        throw err;
      }
    },

    startPolling() {
      this.initializeWallet();
      this.stopPolling();
      this.fetchAccountData();
      this.pollingInterval = setInterval(() => this.fetchAccountData(), 2000);
    },

    stopPolling() {
      if (this.pollingInterval) {
        clearInterval(this.pollingInterval);
        this.pollingInterval = null;
      }
    }
  }
});
