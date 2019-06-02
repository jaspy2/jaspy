import { ActionTree, GetterTree, Module, MutationTree } from 'vuex';

export enum DeployState {
  Stationed,
}

export interface Switch {
  fqdn: string;
  deployState: DeployState;
  configured: boolean;
}

export interface SynchronizationResult {
  added: number;
  existing: number;
  newSwitches: Switch[];
}

export interface SwitchStoreState {
  items: Switch[];
  syncResult?: SynchronizationResult;
  processing: boolean;
  errors: Error[];
}

const moduleState = {
  items: [],
  processing: false,
  errors: [],
} as SwitchStoreState;

const getters = {} as GetterTree<SwitchStoreState, any>;

const actions = {
  async fetch({ commit }) {
    try {
      commit('setProcessing', true);
      const response = await fetch('/api/switch');
      commit('setItems', await response.json());
    } catch (err) {
      commit('addError', err);
    } finally {
      commit('setProcessing', false);
    }
  },
  async synchronize({ commit }) {
    try {
      commit('setProcessing', true);
      const response = await fetch('/api/switch/synchronize', {
        method: 'SYNCHRONIZE',
      });
      commit('setSyncResult', await response.json());
    } catch (err) {
      commit('addError', err);
    } finally {
      commit('setProcessing', false);
    }
  },
} as ActionTree<SwitchStoreState, any>;

const mutations = {
  setProcessing(state, processing: boolean) {
    state.processing = processing;
  },
  addError(state, error: Error) {
    state.errors.push(error);
  },
  setSyncResult(state, result: SynchronizationResult) {
    state.syncResult = result;
  },
  setItems(state, items: Switch[]) {
    state.items = items;
  },
} as MutationTree<SwitchStoreState>;

export default {
  namespaced: true,
  state: moduleState,
  getters,
  actions,
  mutations,
} as Module<SwitchStoreState, any>;
