<template>
  <v-app>
    <v-content>
      <a @click="synchronize">Synchronize</a>
      <SwitchStatePanel v-for="item of switches" switch="item"></SwitchStatePanel>
    </v-content>
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
    })
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
  font-family: 'Avenir', Helvetica, Arial, sans-serif;
  -webkit-font-smoothing: antialiased;
  -moz-osx-font-smoothing: grayscale;
  text-align: center;
  color: #2c3e50;
  margin-top: 60px;
}
</style>
