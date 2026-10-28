pub const ROOT_UID: u32 = 0;
pub const USER_UID: u32 = 1000;
const PASSWORD_MAX: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Credentials { pub uid: u32, pub name: [u8; 8], pub name_len: usize }

#[derive(Clone, Copy)]
struct Account { uid: u32, name: [u8; 8], name_len: usize, salt: [u8; 16], hash: [u8; 32], initialized: bool }

impl Account {
    const fn empty() -> Self { Self { uid: 0, name: [0; 8], name_len: 0, salt: [0; 16], hash: [0; 32], initialized: false } }
}

#[derive(Clone, Copy)]
pub struct AccountStore { accounts: [Account; 2] }

impl AccountStore {
    pub const fn new() -> Self { Self { accounts: [Account::empty(); 2] } }
    pub fn configured(&self) -> bool { self.accounts[0].initialized && self.accounts[1].initialized }

    pub fn configure(&mut self, root_password: &str, user_password: &str, nonce: u64) -> Result<(), ()> {
        if self.configured() || !valid_password(root_password) || !valid_password(user_password) { return Err(()); }
        self.accounts[0] = make_account(ROOT_UID, b"root", root_password, nonce ^ 0x726f6f74);
        self.accounts[1] = make_account(USER_UID, b"user", user_password, nonce ^ 0x75736572);
        Ok(())
    }

    pub fn authenticate(&self, name: &str, password: &str) -> Option<Credentials> {
        for account in self.accounts.iter() {
            if account.initialized && name.as_bytes() == &account.name[..account.name_len] && password.len() <= PASSWORD_MAX && hash_password(&account.salt, password) == account.hash {
                return Some(Credentials { uid: account.uid, name: account.name, name_len: account.name_len });
            }
        }
        None
    }

    pub fn serialize(&self, block: &mut [u8]) {
        block[16] = if self.configured() { 1 } else { 0 };
        for (index, account) in self.accounts.iter().enumerate() {
            let base = 32 + index * 64;
            block[base] = if account.initialized { 1 } else { 0 };
            block[base + 1..base + 5].copy_from_slice(&account.uid.to_le_bytes());
            block[base + 5] = account.name_len as u8;
            block[base + 6..base + 14].copy_from_slice(&account.name);
            block[base + 16..base + 32].copy_from_slice(&account.salt);
            block[base + 32..base + 64].copy_from_slice(&account.hash);
        }
    }

    pub fn deserialize(&mut self, block: &[u8]) -> Result<(), ()> {
        if block[16] == 0 { self.accounts = [Account::empty(); 2]; return Ok(()); }
        for index in 0..2 {
            let base = 32 + index * 64;
            let mut account = Account::empty();
            account.initialized = block[base] == 1;
            account.uid = u32::from_le_bytes([block[base + 1], block[base + 2], block[base + 3], block[base + 4]]);
            account.name_len = (block[base + 5] as usize).min(8);
            account.name.copy_from_slice(&block[base + 6..base + 14]);
            account.salt.copy_from_slice(&block[base + 16..base + 32]);
            account.hash.copy_from_slice(&block[base + 32..base + 64]);
            self.accounts[index] = account;
        }
        if self.accounts[0].uid != ROOT_UID || self.accounts[1].uid != USER_UID { return Err(()); }
        Ok(())
    }
}

fn valid_password(password: &str) -> bool { password.len() >= 8 && password.len() <= PASSWORD_MAX && password.bytes().all(|byte| byte >= 0x20 && byte <= 0x7e) }
fn make_account(uid: u32, name: &[u8], password: &str, nonce: u64) -> Account {
    let mut account = Account::empty(); account.uid = uid; account.name_len = name.len(); account.name[..name.len()].copy_from_slice(name); account.initialized = true;
    for index in 0..16 { account.salt[index] = (nonce.rotate_left((index as u32) * 3) as u8) ^ (index as u8).wrapping_mul(41); }
    account.hash = hash_password(&account.salt, password); account
}

fn hash_password(salt: &[u8; 16], password: &str) -> [u8; 32] {
    let mut message = [0u8; 128]; let length = 16 + password.len(); message[..16].copy_from_slice(salt); message[16..length].copy_from_slice(password.as_bytes()); sha256(&message[..length])
}

fn sha256(input: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [0x428a2f98,0x71374491,0xb5c0fbcf,0xe9b5dba5,0x3956c25b,0x59f111f1,0x923f82a4,0xab1c5ed5,0xd807aa98,0x12835b01,0x243185be,0x550c7dc3,0x72be5d74,0x80deb1fe,0x9bdc06a7,0xc19bf174,0xe49b69c1,0xefbe4786,0x0fc19dc6,0x240ca1cc,0x2de92c6f,0x4a7484aa,0x5cb0a9dc,0x76f988da,0x983e5152,0xa831c66d,0xb00327c8,0xbf597fc7,0xc6e00bf3,0xd5a79147,0x06ca6351,0x14292967,0x27b70a85,0x2e1b2138,0x4d2c6dfc,0x53380d13,0x650a7354,0x766a0abb,0x81c2c92e,0x92722c85,0xa2bfe8a1,0xa81a664b,0xc24b8b70,0xc76c51a3,0xd192e819,0xd6990624,0xf40e3585,0x106aa070,0x19a4c116,0x1e376c08,0x2748774c,0x34b0bcb5,0x391c0cb3,0x4ed8aa4a,0x5b9cca4f,0x682e6ff3,0x748f82ee,0x78a5636f,0x84c87814,0x8cc70208,0x90befffa,0xa4506ceb,0xbef9a3f7,0xc67178f2];
    let mut h: [u32; 8] = [0x6a09e667,0xbb67ae85,0x3c6ef372,0xa54ff53a,0x510e527f,0x9b05688c,0x1f83d9ab,0x5be0cd19];
    let bit_len = (input.len() as u64) * 8; let mut block = [0u8; 128]; block[..input.len()].copy_from_slice(input); block[input.len()] = 0x80; block[119..127].copy_from_slice(&bit_len.to_be_bytes());
    for chunk in block.chunks_exact(64) { let mut w = [0u32; 64]; for i in 0..16 { let p=i*4; w[i]=u32::from_be_bytes([chunk[p],chunk[p+1],chunk[p+2],chunk[p+3]]); } for i in 16..64 { let s0=w[i-15].rotate_right(7)^w[i-15].rotate_right(18)^(w[i-15]>>3); let s1=w[i-2].rotate_right(17)^w[i-2].rotate_right(19)^(w[i-2]>>10); w[i]=w[i-16].wrapping_add(s0).wrapping_add(w[i-7]).wrapping_add(s1); }
        let (mut a,mut b,mut c,mut d,mut e,mut f,mut g,mut hh)=(h[0],h[1],h[2],h[3],h[4],h[5],h[6],h[7]); for i in 0..64 { let s1=e.rotate_right(6)^e.rotate_right(11)^e.rotate_right(25); let ch=(e&f)^((!e)&g); let temp1=hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]); let s0=a.rotate_right(2)^a.rotate_right(13)^a.rotate_right(22); let maj=(a&b)^(a&c)^(b&c); let temp2=s0.wrapping_add(maj); hh=g;g=f;f=e;e=d.wrapping_add(temp1);d=c;c=b;b=a;a=temp1.wrapping_add(temp2); } h[0]=h[0].wrapping_add(a);h[1]=h[1].wrapping_add(b);h[2]=h[2].wrapping_add(c);h[3]=h[3].wrapping_add(d);h[4]=h[4].wrapping_add(e);h[5]=h[5].wrapping_add(f);h[6]=h[6].wrapping_add(g);h[7]=h[7].wrapping_add(hh);
    }
    let mut output=[0u8;32]; for i in 0..8 { output[i*4..i*4+4].copy_from_slice(&h[i].to_be_bytes()); } output
}
