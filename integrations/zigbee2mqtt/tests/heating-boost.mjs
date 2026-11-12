// Run with zigbee-herdsman-converters available to Node.
import assert from 'node:assert/strict';
import definitions from '../nest-learning-thermostat.mjs';
const def=Array.isArray(definitions)?definitions[0]:definitions;
const output=def.toZigbee.find(c=>c.key.includes('heating_boost'));
const reports=def.fromZigbee.find(c=>c.cluster==='genAnalogOutput');
for(const value of ['0','30','60','120','180']) {
 const writes=[];
 const result=await output.convertSet(null,'heating_boost',value,{device:{getEndpoint(id){assert.equal(id,40);return {async write(cluster,attrs){writes.push([cluster,attrs]);}};}}});
 assert.deepEqual(writes,[['genAnalogOutput',{presentValue:Number(value)}]]);
 assert.equal(result.state.heating_boost,value);
 assert.equal(reports.convert(null,{endpoint:{ID:40},data:{presentValue:Number(value)}}).heating_boost,value);
}
console.log('Zigbee2MQTT boost commands and reports: passed');
