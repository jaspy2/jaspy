<template>
  <v-app dark>
      <v-container fluid grid-list-xl>
          <v-layout row wrap>
              <v-flex v-for="item in switches" xs4>
                  <SwitchStatePanel :fqdn="item.fqdn" :deployState="item.deployState" :isConfigured="item.configured" :activePorts="0"></SwitchStatePanel>
              </v-flex>
              <v-flex xs12>
                  <v-btn color="warning" flat @click="synchronize">Synchronize</v-btn>
              </v-flex>
          </v-layout>
      </v-container>
  </v-app>
</template>

<script lang="ts">
import { Component, Vue } from 'vue-property-decorator';
import SwitchStatePanel from '@/components/SwitchStatePanel.vue';
import { mapActions, mapState } from 'vuex';

@Component({
  components: {
    SwitchStatePanel,
  },
  async beforeCreate() {
    await this.$store.dispatch('switch/fetch');
  },
  computed: {
    ...mapState({
      switches: (state: any) => state.switch.items,
      processing: (state: any) => state.switch.processing,
    }),
  },
  methods: {
    ...mapActions('switch', [
      'synchronize',
    ]),
  },
})
export default class App extends Vue {}
</script>

<style lang="scss">
#app {
  font-family: 'Roboto', Helvetica, Arial, sans-serif;
  text-align: center;
  color: #2c3e50;
}
</style>
