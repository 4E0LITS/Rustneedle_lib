/*
Organizational data structures used by the Rustneedle program.
Built as a library so that it can be used as a dependency for
plugins.
*/

use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::time::Duration;
use std::thread::JoinHandle;
use std::sync::{
    Arc,
    Mutex,
    MutexGuard,
    mpsc::Receiver,
    mpsc::Sender,
    mpsc::SendError
};

extern crate libloading;
use libloading::Library;

extern crate pnet;
use pnet::datalink::{
    Config,
    MacAddr,
    ChannelType::Layer2
};

pub const BROADCAST: MacAddr = MacAddr(0xff, 0xff, 0xff, 0xff, 0xff, 0xff); 

pub const DLINKCFG: Config = Config {
    write_buffer_size: 65535,
    read_buffer_size: 65535,
    read_timeout: Some(Duration::from_secs(1)),
    write_timeout: None,
    channel_type: Layer2,
    bpf_fd_attempts: 1000,
};

/// represents an Ip and Mac address pair that must be known
pub struct KnownPair {
    pub proto: Ipv4Addr,
    pub hardw: MacAddr,
}

impl KnownPair {
    pub fn new(ip: Ipv4Addr, hw: MacAddr) -> KnownPair {
        KnownPair {
            proto: ip,
            hardw: hw
        }
    }
}

/// represents a list of pairs that may or may not be known
pub struct NetPairList {
    hosts: Vec<Ipv4Addr>,
    macs: HashMap<Ipv4Addr, Option<MacAddr>>
}

impl NetPairList {
    fn new() -> NetPairList {
        NetPairList {
            hosts: Vec::new(),
            macs: HashMap::new()
        }
    }

    pub fn len(&self) -> usize {
        self.hosts.len()
    }

    pub fn hosts(&self) -> &Vec<Ipv4Addr> {
        &self.hosts
    }

    pub fn macs(&self) -> &HashMap<Ipv4Addr, Option<MacAddr>> {
        &self.macs
    }

    pub fn get(&self, index: usize) -> Option<&Ipv4Addr> {
        self.hosts.get(index)
    }

    pub fn insert(&mut self, host: Ipv4Addr) {
        if self.macs.insert(host, None).is_none() {
            self.hosts.push(host);
        }
    }

    pub fn set_host(&mut self, host: Ipv4Addr, mac: MacAddr) {
        self.macs.insert(host, Some(mac));
    }

}

/// contains safe shared references to hosts on the network
pub struct HostMgr {
    gateway: Arc<Mutex<KnownPair>>,
    myself: Arc<Mutex<KnownPair>>,
    nethosts: Arc<Mutex<NetPairList>>
}

impl HostMgr {
    pub fn new(gate: KnownPair, me: KnownPair) -> HostMgr {
        HostMgr {
            gateway: Arc::new(Mutex::new(gate)),
            myself: Arc::new(Mutex::new(me)),
            nethosts: Arc::new(Mutex::new(NetPairList::new()))
        }
    }

    pub fn get_gateway(&self) -> Arc<Mutex<KnownPair>> {
        self.gateway.clone()
    }

    pub fn get_myself(&self) -> Arc<Mutex<KnownPair>> {
        self.myself.clone()
    }

    pub fn get_nethosts(&self) -> Arc<Mutex<NetPairList>> {
        self.nethosts.clone()
    }

    //pub fn acquire(&mut self) -> (MutexGuard<>)

    pub fn acquire_gateway(&mut self) -> MutexGuard<KnownPair> {
        self.gateway.lock().unwrap()
    }

    pub fn acquire_myself(&mut self) -> MutexGuard<KnownPair> {
        self.myself.lock().unwrap()
    }

    pub fn acquire_nethosts(&mut self) -> MutexGuard<NetPairList> {
        self.nethosts.lock().unwrap()
    }
}

impl Clone for HostMgr {
    fn clone(&self) -> Self {
        HostMgr {
            gateway: self.gateway.clone(),
            myself: self.myself.clone(),
            nethosts: self.nethosts.clone()
        }
    }
}

/*
Here's where it gets interesting. The meat of this program is the hook system. Hooks are defined
either by dylibs loaded at runtime or from within the core, and represent something the user can
run from the command line. When a hook is loaded, it comes with a name String that is mapped
to it within the Framework. A hook is simply a function that accepts a reference to a collection
of arg strings and environment information, then returns either success or failure. On success,
a hook has the option of returning a Module, a threaded addition which will run in the background.
A module has the option of being continually handed shared references to packets or parts of
packets based on a PackFilter. When packets come in over the network, references for the packet's
ethernet header and payload are generated, then handed to waiting modules, allowing the modules to
read them and queue packet creation requests as needed.

Modules have some additional features built in: each has an mpsc::Receiver<()> whose intent is to
be able to kill the module and cause cleanup. Each is also initialized with the option of an
mpsc::Sender(vec<u8>) that can be used to queue packet send requests.

Hooks are organized by what level information they need. Some may only need access to the HostMgr,
while others may require framework level access.
*/

pub struct Module {
    handle: JoinHandle<Result<(), String>>,
    killer: Sender<()>
}

impl Module {
    pub fn new(
        handle: JoinHandle<Result<(), String>>,
        killer: Sender<()>,
    ) -> Module {
        Module {
            handle: handle,
            killer: killer
        }
    }

    pub fn handle(&mut self) -> &mut JoinHandle<Result<(), String>> {
        &mut self.handle
    }

    pub fn kill(&mut self) -> Result<(), SendError<()>> {
        self.killer.send(())
    }
}

pub enum Hook {
    Framework((fn(&[&str], &mut Framework) -> Result<Option<Module>, String>)),
    HostMgr((fn(&[&str], &mut HostMgr) -> Result<Option<Module>, String>))
}

type HookLoader = unsafe fn() -> Vec<(&'static str, Hook)>;

pub struct Framework {
    running: bool,
    module_drop: Option<Sender<Sender<Arc<Vec<u8>>>>>, // drop module's packet receiver to packets be passed to module
    packet_queue: Option<Sender<Vec<u8>>>, // send packets to this to have them be sent to net
    libraries: Vec<Library>,
    hosts: HostMgr,
    names: Vec<&'static str>,
    hooks: HashMap<String, Hook>,
    modules: HashMap<String, Module>
}

impl Framework {
    pub fn new(hostmgr: HostMgr) -> Framework {
        Framework {
            running: true,
            module_drop: None,
            packet_queue: None,
            libraries: Vec::new(),
            hosts: hostmgr,
            names: Vec::new(),
            hooks: HashMap::new(),
            modules: HashMap::new()
        }
    }

    // external field reference access

    pub fn hosts(&self) -> &HostMgr {
        &self.hosts
    }

    pub fn names(&self) -> &Vec<&'static str> {
        &self.names
    }

    pub fn hooks(&self) -> &HashMap<String, Hook> {
        &self.hooks
    }

    pub fn modules(&self) -> &HashMap<String, Module> {
        &self.modules
    }

    pub fn is_running(&self) -> bool {
        self.running
    }   

    pub fn stop(&mut self) {
        self.running = false
    }

    // util

    pub fn hook_up(&mut self, name: &'static str, hook: Hook) -> Result<(), ()> {
        if !self.hooks.contains_key(name) {
            self.names.push(name);
            self.hooks.insert(String::from(name), hook);
            Ok(())
        } else {
            Err(())
        }
    }

    pub fn load_hooks_from(&mut self, lib: Library) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();
        self.libraries.push(lib);

        let hooks = unsafe { match self.libraries.last().unwrap().get::<HookLoader>(b"load") {
            Ok(load) => load(),
            Err(e) => return Err(vec![e.to_string()])
        }};

        for (name, hook) in hooks.into_iter() {
            if self.hook_up(name, hook).is_err() {
                errors.push(format!("{} already bound", name));
            }
        };

        if errors.len() > 0 {
            Err(errors)
        } else {
            Ok(())
        }
    }

    pub fn try_run_hook(&mut self, name: &str, args: &[&str]) -> Result<(), String> {
        let mut name = name.to_owned();

        if self.hooks.contains_key(&name) {
            match match self.hooks[&name] {
                Hook::Framework(func) => func(args, self),
                Hook::HostMgr(func) => func(args, &mut self.hosts)
            } {
                Ok(modopt) => match modopt {
                    Some(module) => {
                        // if name in use, find an acceptable name for new instance by incrementing
                        let mut counter = 0;

                        while self.modules.contains_key(&name) {
                            name = format!("{}_{}", name, counter);
                            counter += 1;
                        }

                        println!("[*] Started '{}'", name);
                        self.modules.insert(name.clone(), module);
                        Ok(())
                    },

                    None => Ok(())
                },

                /*
                because this Err is a Result<Option<...>, String> and not a Result<(), String>, rustc won't let me just
                use "e => e". The Ok(Option<...>) has already been matched, so any other result would HAVE to be an
                Err(string) since that's only other type left in the enum. But because this Err(String) is "from" a
                different kind of Result, apparently I have to get the innter String out of the Err and then re-wrap it.
                I'm sure there's a way to do this that doesn't look fucking stupid, but I can't think of the it atm.
                */
                Err(s) => Err(s)
            }
        } else {
            Err(format!("{}: No such hook", name))
        }
    }

    pub fn init_task_mpscs(&mut self, mod_drop: Sender<Sender<Arc<Vec<u8>>>>, sender: Sender<Vec<u8>>) {
        self.module_drop = Some(mod_drop);
        self.packet_queue = Some(sender);
    }

    pub fn try_kill(&mut self, name: &str) -> Result<(), String> {
        if let Some(mut module) = self.modules.remove(name) {
            if module.kill().is_err() {
                Err(format!("{}: thread has already died", name))
            } else {
                Ok(())
            }

        } else {
            Err(format!("{}: No such module", name))
        }
    }

    pub fn insert_packet_tx(&mut self, packet_tx: Sender<Arc<Vec<u8>>>) -> Result<(), ()> {
        match &self.module_drop {
            Some(drop) => Ok(drop.send(packet_tx).unwrap()),
            None => Err(())
        }
    }

    pub fn get_packet_queue(&self) -> Option<Sender<Vec<u8>>> {
        self.packet_queue.clone()
    }
}