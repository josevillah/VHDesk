//! Reciclado de los buffers de pixeles.
//!
//! Un frame de 1080p en BGRA ocupa 8,3 MiB. A 60 fps, asignar y liberar uno por frame
//! son ~500 MB/s de trasiego que dominarian el perfil y fragmentarian el heap, asi que
//! el capturador es dueno de sus buffers y los recicla.
//!
//! El mecanismo: [`BufferPool::take`] saca un `Vec<u8>` de la lista libre (o asigna uno
//! si no queda ninguno) y lo entrega envuelto en un [`PooledBuffer`]. Cuando el consumidor
//! suelta ese handle, su `Drop` devuelve el `Vec` a la lista. El handle es `Send`, asi que
//! puede cruzar el canal que separa la captura del encoder sin copiar los pixeles.
//!
//! La lista tiene un tope de elementos retenidos: si el consumidor se queda con muchos
//! frames a la vez, se asignan buffers nuevos, pero al devolverlos solo se conservan unos
//! pocos. Asi un consumidor lento no convierte el pool en una fuga de memoria.

use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Mutex};

/// Lista de buffers de pixeles reutilizables.
///
/// Clonar el pool es barato y comparte la misma lista: el capturador se queda una copia y
/// cada [`PooledBuffer`] lleva otra para poder devolverse solo.
#[derive(Debug, Clone)]
pub struct BufferPool {
    free: Arc<Mutex<Vec<Vec<u8>>>>,
    max_retained: usize,
}

impl BufferPool {
    /// Crea un pool que conserva como mucho `max_retained` buffers libres.
    pub fn new(max_retained: usize) -> Self {
        Self {
            free: Arc::new(Mutex::new(Vec::new())),
            max_retained: max_retained.max(1),
        }
    }

    /// Entrega un buffer de exactamente `len` bytes.
    ///
    /// El contenido es indeterminado: quien lo recibe debe escribirlo entero. En regimen
    /// estacionario, con todos los frames del mismo tamano, esto no asigna ni recorre la
    /// memoria, porque el `Vec` reciclado ya tiene la longitud correcta.
    pub fn take(&self, len: usize) -> PooledBuffer {
        let mut data = match self.free.lock() {
            Ok(mut free) => free.pop().unwrap_or_default(),
            // Un mutex envenenado solo puede venir de un panico en otro hilo mientras
            // manipulaba la lista. Perder el reciclado es preferible a propagar el panico:
            // la captura sigue funcionando, solo que asignando.
            Err(_) => Vec::new(),
        };

        if data.len() != len {
            data.resize(len, 0);
        }

        PooledBuffer {
            data,
            free: Arc::clone(&self.free),
            max_retained: self.max_retained,
        }
    }

    /// Numero de buffers libres ahora mismo. Solo para tests y diagnostico.
    pub fn free_count(&self) -> usize {
        self.free.lock().map(|free| free.len()).unwrap_or(0)
    }
}

/// Buffer de pixeles prestado por un [`BufferPool`], que vuelve a el al soltarse.
#[derive(Debug)]
pub struct PooledBuffer {
    data: Vec<u8>,
    free: Arc<Mutex<Vec<Vec<u8>>>>,
    max_retained: usize,
}

impl Drop for PooledBuffer {
    fn drop(&mut self) {
        let data = std::mem::take(&mut self.data);
        if data.capacity() == 0 {
            return;
        }
        if let Ok(mut free) = self.free.lock() {
            if free.len() < self.max_retained {
                free.push(data);
            }
        }
    }
}

impl Deref for PooledBuffer {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        &self.data
    }
}

impl DerefMut for PooledBuffer {
    fn deref_mut(&mut self) -> &mut [u8] {
        &mut self.data
    }
}

impl AsRef<[u8]> for PooledBuffer {
    fn as_ref(&self) -> &[u8] {
        &self.data
    }
}

#[cfg(test)]
mod tests {
    use super::BufferPool;

    #[test]
    fn un_buffer_soltado_vuelve_al_pool() {
        let pool = BufferPool::new(4);
        assert_eq!(pool.free_count(), 0);

        let buffer = pool.take(1024);
        assert_eq!(buffer.len(), 1024);
        assert_eq!(pool.free_count(), 0, "mientras esta prestado no esta libre");

        drop(buffer);
        assert_eq!(pool.free_count(), 1);
    }

    #[test]
    fn el_pool_reutiliza_la_misma_asignacion() {
        let pool = BufferPool::new(4);

        let mut primero = pool.take(4096);
        primero[0] = 0xab;
        let direccion = primero.as_ptr();
        drop(primero);

        let segundo = pool.take(4096);
        assert_eq!(
            segundo.as_ptr(),
            direccion,
            "el segundo take deberia reciclar el buffer del primero, no asignar otro"
        );
    }

    #[test]
    fn el_pool_no_retiene_mas_de_lo_permitido() {
        let pool = BufferPool::new(2);

        let buffers: Vec<_> = (0..5).map(|_| pool.take(64)).collect();
        assert_eq!(pool.free_count(), 0);

        drop(buffers);
        assert_eq!(
            pool.free_count(),
            2,
            "un consumidor que retenga muchos frames no debe hacer crecer el pool"
        );
    }

    #[test]
    fn cambiar_de_tamano_ajusta_la_longitud() {
        let pool = BufferPool::new(2);

        drop(pool.take(100));
        let grande = pool.take(500);
        assert_eq!(grande.len(), 500);
        drop(grande);

        let pequeno = pool.take(10);
        assert_eq!(pequeno.len(), 10);
    }
}
